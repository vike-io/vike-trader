//! Which accounts arm: the pre-assembly live set, the mount accounts, both refusals, the fan-out.

use std::collections::HashSet;

use vike_core::CoreConfig;

#[cfg(doc)]
use super::RuntimeAssembly;
#[cfg(doc)]
use super::RuntimeMarket;
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
/// * `vike-app` locked the RIGHT set ([`RuntimeAssembly::live_venues`]) at the WRONG TIME: it exists only
///   after [`assemble_runtime`] built every exec client, and three arms
///   (`crates/bridges/bybit/src/execution/mod.rs`, `crates/bridges/okx/src/execution/mod.rs`,
///   `crates/bridges/aster/src/execution/mod.rs` — each posts `set_leverage` at startup) make a
///   post-construction refusal no longer side-effect-free.
///
/// # Direction of error
///
/// The probe is INTENT-based, so this set can be a strict SUPERSET of the [`RuntimeAssembly::live_venues`] the
/// mount records: a venue whose synchronous connect fails (ctrader/ibkr) or whose factory declines
/// a present-but-bad key (hyperliquid/polymarket) probes live and then does not run — one extra
/// sentinel, a SECOND process refused there. The other direction, a venue arming with nothing
/// holding its lock, is what this seam prevents; [`refuse_unarmed_live_venues`] is its backstop.
///
/// # ⚠ It answers ROUTE KEYS, one per ACCOUNT — not venue ids
///
/// The lock protects a venue ACCOUNT, and the mount fans out per account. A route key IS that
/// identity — `vike_execution::Engine::route_key`, the name `make_engine_accounts` records in
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
pub struct MountRoute {
    /// The mount's venue — a `vike_model::VENUES` id.
    pub venue: String,
    /// The instrument this mount trades, which is the symbol its ACCOUNT is armed on.
    pub symbol: String,
    /// Which account of that venue, `None` for the venue's default one.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
}

/// The [`MountRoute`] rows a built [`CoreConfig`] declares, read back off the config so
/// [`assemble_runtime`]'s named-account refusal and per-account symbols judge the very mount set it is
/// about to assemble.
#[must_use]
pub fn runnable_accounts(core: &CoreConfig) -> Vec<MountRoute> {
    core.strategy
        .iter()
        .chain(core.extra_mounts.iter())
        .map(|m| MountRoute {
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
            account: m.account.clone(),
        })
        .collect()
}

/// **A strategy mount that names no account keeps meaning the venue's DEFAULT account when a lane twin
/// runs beside it.** `engines` is `(venue, route key)` of every engine this core is spawned with.
///
/// The core refuses an account-less mount on a venue it runs more than one engine of (it will not pick:
/// `vike_core`'s `mount_engine_resolution`), and a derived twin (`vike_venues::lane_twin`) makes its venue
/// such a venue. Every shipped mount is account-less and means the default account (the spot engine it was
/// written for), so stating a margin leverage on the account would have refused every strategy on it at the
/// next start. An explicit `DEFAULT` names the unlabelled account and routes to it at any engine count,
/// which is the engine the account-less spelling reached before the twin existed: nothing else changes.
/// A mount that names an account is left as it is, and so is every venue without a DERIVED twin
/// (`AccountDirectory::is_derived_lane_twin`, not the label).
pub(super) fn name_the_default_account_where_a_lane_twin_runs(
    core: &mut CoreConfig,
    engines: &[(String, String)],
    accounts: &vike_secrets::account_directory::AccountDirectory,
) {
    let twin_venues: Vec<&str> = engines
        .iter()
        .filter(|(venue, key)| {
            vike_model::accounts::account_keys::label_of_route_key(venue, key).is_some_and(
                |label| label.text().is_some_and(|text| accounts.is_derived_lane_twin(venue, text)),
            )
        })
        .map(|(venue, _)| venue.as_str())
        .collect();
    if twin_venues.is_empty() {
        return;
    }
    for mount in core.strategy.iter_mut().chain(core.extra_mounts.iter_mut()) {
        if mount.account.is_none() && twin_venues.contains(&mount.venue.as_str()) {
            mount.account = Some(vike_model::accounts::account_keys::AccountLabel::Default);
        }
    }
}

/// **The node's steps over the journaled engines before the core restores them**
/// (`vike_core::CoreConfig::restored_engine_steps`): [`adopt_stated_currency_in_restored_engines`], then
/// [`restore_spot_positions_cash`]. The core runs them wherever it restores a book, at the assembly over the booted
/// engines and at an account's late join over its own (`crates/vike-core/src/runtime/late_join.rs`): one path.
pub(super) fn restored_engine_steps<C: vike_execution::ExecutionClient>(
    snaps: &mut [vike_execution::EngineSnapshot],
    engines: &[&vike_execution::Engine<C>],
) {
    adopt_stated_currency_in_restored_engines(snaps, engines);
    restore_spot_positions_cash(snaps, engines);
}

/// **A journaled engine whose ledger label was the constructor's GUESS is restored under the label the engine now
/// states, when the numbers it journaled are already in that currency** (decision 0183, item 1).
///
/// `assemble_engine` tells an engine of a converting account its currency (`set_account_currency`), which relabels it from
/// the `"USDT"` guess to `"USD"`. `vike_core`'s `apply_restored_engines` SKIPS the journal restore of a DEFAULT
/// account's engine (a snapshot with no route key) whose `quote_asset` differs from the live engine's, so the first
/// restart after the relabel came up flat: harmless where a reconcile client re-seeds the book, a position and a
/// resting order lost from the books of a venue whose reconcile leg holds and never folds (`Wiring::SidecarExecutionReconcile`:
/// dukascopy under `quarantine`), whose venue keeps them. The core is not changed for it (it is measured); the snapshot is told the label instead, here, before
/// the core reads it.
///
/// The adoption is made only when it cannot misstate a number: the snapshot never named a currency (the guess, not
/// told), the engine mounts the same symbol, and that symbol is QUOTED in the engine's currency, as its KEPT row states
/// or, with no row stating one, as a six-letter pair's name does (`crate::engine::mount_money`: ibkr's `AAPL` whose row
/// says `USD`, `EURUSD` for `USD`), so what the old run booked at par was already in it. A snapshot that was told its
/// currency, a labelled account's (it is matched by route key and never skipped), an engine mounted on a symbol in
/// another money than its stated currency and one whose money nothing states (an ibkr stock with no row) are left as
/// they were: the core skips those as before, at WARN, and at ERROR naming what the book held when it held a position
/// or a resting order. The next snapshot the core writes carries the new label, so the restart after this one matches
/// on its own. (the CI box, 2026-11-17: ibkr's `AAPL` engine journaled under the guess `USDT` while ibkr had no kept list,
/// and the next start, anchored to the account's `USD` over the row the operator added since, skipped it.)
///
/// ⚠ It vouches for the UNITS, not for whose state the snapshot is, except for the account's TIER: a snapshot
/// carries the tier of the engine that journaled it (`EngineSnapshot::tier`), and one whose tier is `Some(t)` with
/// `t` another than the live engine's is NOT adopted (a `paper` run's snapshot is not handed to the demo engine of
/// the same venue after a tier flip: the core's restore refuses it by the same field). `None` is a snapshot
/// journaled before the field existed, whose tier is unknown, and it is adopted as it always was. It carries no
/// account identity beyond the route key: the boot restore matches by route key, venue and, for a default
/// account, market, and a venue with a reconcile client re-seeds its book from the venue.
pub(super) fn adopt_stated_currency_in_restored_engines<C: vike_execution::ExecutionClient>(
    snaps: &mut [vike_execution::EngineSnapshot],
    engines: &[&vike_execution::Engine<C>],
) {
    for snap in snaps.iter_mut().filter(|s| s.route_key.is_none()) {
        let Some(engine) =
            engines.iter().find(|e| e.route_key == snap.venue && e.venue == snap.venue)
        else {
            continue;
        };
        if !engine.account_currency_known()
            || snap.symbol != engine.symbol
            || snap.tier.is_some_and(|tier| tier != engine.tier)
            || snap.account_currency_set
            // The label was the constructor's guess, not a statement.
            || snap.quote_asset != vike_execution::execution_engine::DEFAULT_QUOTE_ASSET
            || snap.quote_asset == engine.quote_asset
            // The mount's money as the anchor fence reads it (its kept row's, else a six-letter pair's), asked last:
            // it looks the row up.
            || !crate::engine::mount_money(&engine.venue, &engine.symbol)
                .is_some_and(|money| money.eq_ignore_ascii_case(&engine.quote_asset))
        {
            continue;
        }
        tracing::info!(
            venue = %snap.venue,
            journaled = %snap.quote_asset,
            now = %engine.quote_asset,
            "the journaled {} engine never stated its currency (its ledger label was the guess) and its mounted \
             symbol {} is quoted in {}: its state is restored under {}",
            snap.venue,
            engine.symbol,
            engine.quote_asset,
            engine.quote_asset
        );
        snap.quote_asset.clone_from(&engine.quote_asset);
    }
}

/// **A journaled position of a SPOT pair on a spot mount is restored `Cash`**, whatever mode the run that journaled
/// it booked it in.
///
/// `assemble_engine` states `Cash` for a spot mount's symbol, but an entry that already exists keeps the mode it was
/// born in (`vike_execution::Account`'s fold reads the grids only on a miss, a flat entry included), so a book journaled
/// before that statement would keep its `Cross` entry, and the margin-call sweep its false breach against a wallet
/// net of the holding's cost, for good. As [`adopt_stated_currency_in_restored_engines`] tells the snapshot a label,
/// this tells it the mode, before the core reads it: the core is measured and is not changed for it. Only the snapshot
/// of an engine that states `Cash` for its own symbol (matched by route key and venue, as the core's restore matches:
/// a `paper` engine states none) is read, and in it only the entries `vike_catalog::books_cash` names move (a spot
/// pair, or every entry on a venue whose ONLY margin mode is `Cash`: polymarket's outcome shares); every other entry,
/// snapshot and engine is left as it was. Idempotent.
pub(super) fn restore_spot_positions_cash<C: vike_execution::ExecutionClient>(
    snaps: &mut [vike_execution::EngineSnapshot],
    engines: &[&vike_execution::Engine<C>],
) {
    for snap in snaps.iter_mut() {
        let key = snap.route_key.as_deref().unwrap_or(&snap.venue);
        if !engines.iter().any(|e| {
            e.route_key == key
                && e.venue == snap.venue
                && e.account.default_margin_mode_of(&e.symbol) == vike_model::MarginMode::Cash
        }) {
            continue;
        }
        for ((_, symbol, _), entry) in &mut snap.account.positions {
            if entry.margin_mode.is_cross()
                && vike_catalog::books_cash(&snap.venue, symbol.as_str())
            {
                entry.margin_mode = vike_model::MarginMode::Cash;
            }
        }
    }
}

/// **WHICH SYMBOL each account of `venue` is armed on**, so a labelled account's ENGINE mounts on
/// the instrument its own strategy trades.
///
/// ⚠ Two accounts may land on the SAME symbol: that is a spread (`vike_config::venue_accounts`),
/// and nothing about this map is a uniqueness claim.
///
/// * the DEFAULT account is on the symbol its row STATES (`vike_config::account_arming::account_symbol`:
///   `account.symbol` of its active row; owner, 2026-11-13, decision 0156's amendment), even if a mount
///   names it on another instrument: `vike_tradehub::config::ProfileDaemon::validate_for_live` refuses an
///   account-less live row naming anything else. A default row that states none gets NO entry: that
///   account does not run (`ArmingBlock::NoAccountSymbol`), and no symbol is chosen for it;
/// * one entry per DISTINCT labelled account a mount on this venue names, on that mount's own
///   `symbol` — the mount decides the instrument and names the account, so no settings key
///   restates it;
/// * a labelled account NO mount names is mounted on the symbol its ACTIVE account row STATES
///   (`vike_secrets::AccountRow::symbol`, `vike-cli account set-symbol`); the mount, when there
///   is one, still decides. The boot and the account watcher's hot start (decision 0169) both reach
///   this through the rows they just read (`accounts`), so activating such a row starts its engine on
///   its own symbol;
/// * a labelled account that neither a mount names nor its row states a symbol for gets no entry, and
///   does not run (`ArmingBlock::NoAccountSymbol`): nothing falls back to another account's symbol;
/// * a venue's LANE TWIN (`vike_venues::lane_twin`: the second engine over the default account's
///   row, for the exec lane its default engine does not trade) gets an entry on the symbol its DERIVED
///   row states (the parent's `lane_twin_symbol` fact, on the twin's lane), unless a mount already
///   named that label, and ONLY when `accounts` DERIVED it (`AccountDirectory::is_derived_lane_twin`);
///   an operator's own `PERP` account is an ordinary labelled row above;
/// * mounts on OTHER venues contribute nothing.
///
/// Order: `Default` first (load-bearing: `crate::make_engine_accounts` returns its engine at `[0]`
/// and callers bind it), then labelled accounts in DECLARATION order, then the stated symbols in
/// row order, then the lane twin.
///
/// ⚠ **Two mounts naming ONE labelled account on TWO symbols is a LOAD refusal**
/// (`vike_tradehub::config::ProfileDaemon`'s multi-mount validation names both rows); this function
/// takes the FIRST mount's symbol only so that it is total.
#[must_use]
pub fn account_symbols_for(
    mounts: &[MountRoute],
    venue: &str,
    accounts: &vike_secrets::account_directory::AccountDirectory,
) -> Vec<(vike_model::accounts::account_keys::AccountLabel, String)> {
    use vike_config::account_arming::account_symbol;
    use vike_model::accounts::account_keys::AccountLabel;
    let mut out: Vec<(AccountLabel, String)> =
        account_symbol(Some(accounts), venue, &AccountLabel::Default)
            .map(|symbol| (AccountLabel::Default, symbol.to_string()))
            .into_iter()
            .collect();
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
    // The symbol a labelled ACTIVE row states for its engine (the default row's is above, a derived lane twin's
    // below).
    let rows = accounts.rows().and_then(Result::ok).and_then(vike_secrets::AccountRows::known);
    for row in rows.unwrap_or_default().iter().filter(|a| a.active && a.venue == venue) {
        let (Some(text), Some(symbol)) = (row.label.as_deref(), row.symbol.as_deref()) else {
            continue;
        };
        if accounts.is_derived_lane_twin(venue, text) {
            continue;
        }
        if let Ok(label) = AccountLabel::parse(text)
            && !out.iter().any(|(l, _)| *l == label)
        {
            out.push((label, stated_engine_symbol(row, symbol)));
        }
    }
    if let Some(twin) = vike_venues::lane_twin(venue)
        && accounts.is_derived_lane_twin(venue, twin.label)
        && let Ok(label) = AccountLabel::parse(twin.label)
        && !out.iter().any(|(l, _)| *l == label)
        && let Some(symbol) = account_symbol(Some(accounts), venue, &label)
    {
        out.push((label, symbol.to_string()));
    }
    out
}

/// **A row's stated symbol as `vike-cli account set-symbol` stores one** (`vike_catalog::venue_engine_symbol`:
/// the user name the venue's KEPT row answers, decision 0290, `vike_catalog::kept_list_now` read at this cold start; the
/// C-D4 follow-up): a row written any other way (by hand, or in another case) is mounted on the user name,
/// `btcusdt.p` on `BTCUSDT.P`. A name the door or the kept list refuses (an old spelling, no list kept: its words name
/// the refresh) is mounted as stated, as before, and said at `error!` in its words: the adapter's start then answers
/// it, and an account it parks is asked again when the venue's list is written (`vike-tradehub`'s account watcher).
fn stated_engine_symbol(row: &vike_secrets::AccountRow, symbol: &str) -> String {
    let kept = vike_catalog::kept_list_now(&row.venue).unwrap_or_default();
    vike_catalog::venue_engine_symbol(&row.venue, symbol, &kept).unwrap_or_else(|why| {
        tracing::error!(
            venue = %row.venue, account = row.id, %symbol,
            "the account's stated symbol is not one `account set-symbol` would store: {why}"
        );
        symbol.to_string()
    })
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
///
/// ⚠ **Not for an account whose VENUE is not connected** (owner 2026-11-14, decision 0156's 2026-11-14 amendment:
/// `vike_model::ArmingBlock::venue_not_connected`): that mount passes here and is DEACTIVATED after the build.
pub(super) fn refuse_unarmed_mount_accounts(
    config: &RuntimeConfig,
    table: &crate::ArmingTable,
    mounts: &[MountRoute],
) -> Result<(), RuntimeError> {
    for m in mounts {
        let label =
            m.account.clone().unwrap_or(vike_model::accounts::account_keys::AccountLabel::Default);
        let rows = crate::venue_account_arming(table, &m.venue, Some(&config.policy));
        // A venue the roster does not carry (a test's planted id) has no rows to judge by: the
        // mount itself answers for it, and the post-build check refuses what did not run.
        if rows.is_empty() {
            continue;
        }
        match rows.iter().find(|row| row.label == label) {
            Some(row) if row.runs() => {}
            // Its VENUE is not connected (the gatewayhub or the price path away): the mount is deactivated after
            // the build, never the node refused ([`refuse_mounts_without_an_engine`]).
            Some(row) if row.block.venue_not_connected() => {}
            row => {
                let why = row.map_or("no account row names it", |r| r.block.as_str());
                return Err(RuntimeError::Mount(unrunnable_mount(m, &label, why)));
            }
        }
    }
    Ok(())
}

/// **A strategy mount the start DEACTIVATED** (owner 2026-11-14: "a strategy can trade only on CONNECTED venues"):
/// its account does not run because its venue is not connected (`vike_model::ArmingBlock::venue_not_connected`), so
/// the node started without it. Kept, built, by [`super::HotAccounts`] and handed to the core
/// (`vike_execution::AccountChange::AddMounts`) right after the account's engine joins: it starts BY ITSELF when the
/// account starts.
pub(crate) struct DeactivatedMount {
    /// The route key of the account it trades.
    pub(crate) route_key: String,
    /// The mount, as the boot built it.
    pub(crate) mount: vike_core::Mount,
}

/// The words a deactivated mount is told in: `venue fxcm not connected: FX market closed: login at ...`.
pub(super) fn deactivated_words(venue: &str, why: &str) -> String {
    format!("venue {venue} not connected: {why}")
}

/// **Take the deactivated mounts out of `core`** (`at`: their indices in `strategy` then `extra_mounts`, with the
/// words), keeping every other mount in its order. When the first is taken, the next mount kept becomes the core's
/// `strategy`. `at` empty leaves `core` exactly as it was.
///
/// A mount whose strategy file is still building (`CoreConfig::mounts_awaiting_strategy`) is KEPT: the core parks
/// it, and once its strategy arrives it waits there for its account (`vike_core`'s `add_mounts_hot`), so its
/// placeholder is never what the account's start mounts.
pub(super) fn take_deactivated_mounts(
    core: &mut CoreConfig,
    at: &[(usize, String)],
) -> Vec<DeactivatedMount> {
    if at.is_empty() {
        return Vec::new();
    }
    let all: Vec<vike_core::Mount> =
        core.strategy.take().into_iter().chain(core.extra_mounts.drain(..)).collect();
    let (mut kept, mut taken) = (Vec::new(), Vec::new());
    for (i, mount) in all.into_iter().enumerate() {
        let awaiting =
            core.mounts_awaiting_strategy.contains(&vike_core::strategy_state::mount_id_with(
                mount.controller_id.as_deref(),
                &mount.venue,
                &mount.symbol,
                &mount.interval,
            ));
        match at.iter().find(|(j, _)| *j == i && !awaiting) {
            Some((_, reason)) => {
                let label = mount
                    .account
                    .clone()
                    .unwrap_or(vike_model::accounts::account_keys::AccountLabel::Default);
                let route_key =
                    vike_model::accounts::account_keys::route_key_of(&mount.venue, &label);
                tracing::error!(
                    venue = %mount.venue, route_key = %route_key,
                    "ARMING: the strategy mount on {}/{} @ {} is DEACTIVATED: {reason}. The node runs \
                     without it, every other mount runs, its account's orders are refused with that \
                     reason, and it starts by itself when its account starts",
                    mount.venue, mount.symbol, mount.interval
                );
                taken.push(DeactivatedMount { route_key, mount });
            }
            None => kept.push(mount),
        }
    }
    let mut kept = kept.into_iter();
    core.strategy = kept.next();
    core.extra_mounts = kept.collect();
    taken
}

/// **THE POST-BUILD HALF of [`refuse_unarmed_mount_accounts`]**: every strategy mount's account
/// has an ENGINE, or the node is refused — naming the account and the reason the mount gave
/// (`not_running`), before `spawn_core_multi` would panic on the same miss. Reached by an account
/// the probe armed whose mount did not come up, and by a planted non-roster venue the projection
/// cannot judge.
///
/// ⚠ **A mount whose account's VENUE is not connected is DEACTIVATED, not refused** (owner 2026-11-14,
/// `vike_model::ArmingBlock::venue_not_connected`: the FX market closed, the gatewayhub away, a session that
/// did not come up, ...): its index in `mounts` comes back with the words ([`deactivated_words`]), for
/// [`take_deactivated_mounts`]. Every other cause still refuses the node.
pub(super) fn refuse_mounts_without_an_engine(
    mounts: &[MountRoute],
    engine_route_keys: &[&str],
    not_running: &[crate::ArmingNotRunning],
) -> Result<Vec<(usize, String)>, RuntimeError> {
    use vike_model::accounts::account_keys::AccountLabel;
    let mut deactivated = Vec::new();
    for (i, m) in mounts.iter().enumerate() {
        let label = m.account.clone().unwrap_or(AccountLabel::Default);
        let key = vike_model::accounts::account_keys::route_key_of(&m.venue, &label);
        if engine_route_keys.contains(&key.as_str()) {
            continue;
        }
        let held = not_running.iter().find(|nr| nr.route_key() == key);
        if let Some(nr) = held.filter(|nr| nr.block.venue_not_connected()) {
            deactivated.push((i, deactivated_words(&m.venue, &nr.view().reason)));
            continue;
        }
        let why = held.map_or("this runtime builds no engine for it", |nr| nr.block.as_str());
        return Err(RuntimeError::Mount(unrunnable_mount(m, &label, why)));
    }
    Ok(deactivated)
}

/// The refusal sentence for a strategy mount whose account does not run.
fn unrunnable_mount(
    m: &MountRoute,
    label: &vike_model::accounts::account_keys::AccountLabel,
    why: &str,
) -> String {
    let key = vike_model::accounts::account_keys::route_key_of(&m.venue, label);
    if label.is_default() {
        format!(
            "strategy mount on {}/{} trades {}'s DEFAULT account, which does NOT RUN on this box \
             ({why}) — no `{key}` engine exists, and an account that does not run is never \
             replaced by a paper engine (decision 0156). Run it: `vike-cli account ls` to \
             find its row, then `vike-cli account activate --id N` / `set-tier --id N \
             --tier <paper|demo|live>`, plus its credential keys for a demo or live tier; \
             `vike-backend venues` names the block. Or drop the mount.",
            m.venue, m.symbol, m.venue
        )
    } else {
        format!(
            "strategy mount on {}/{} names account `{label}`, which does NOT RUN on this box ({why}) \
             — so no `{key}` engine exists, and the mount will not run on {}'s DEFAULT account, \
             trading a book its author did not choose. Run it: `vike-cli account ls` to find \
             its row, then `vike-cli account activate --id N` (and `set-tier`), plus that \
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
    pub engines:
        Vec<(f64, vike_execution::Engine<Box<dyn vike_execution::ExecutionClient + Send>>)>,
    pub(super) reconcile: Vec<crate::ReconcileLeg>,
    pub(super) not_running: Vec<crate::ArmingNotRunning>,
}

/// Mount every account of one wired market that RUNS, hand back the DEFAULT account's engine
/// (placed in the core by the row's `engine_rank`) — `None` when the default account does not run —
/// and push the rest into `extras`, the accounts that do not run included.
///
/// ⚠ `crate::make_engine_accounts` guarantees the default account is FIRST when it runs; with one
/// account per venue `extras` never grows an engine.
///
/// Each extra account's reconcile handle becomes a `vike_tradehub_core::ReconcileLeg` carrying TWO facts: the
/// CANONICAL venue id and that account's route key (`venue#LABEL`). The fold thread routes the pass
/// by route key; every venue-shaped job — the health probe, the ring notes, the held-alert dedup
/// identity — reads a real `vike_model::VENUES` id.
///
/// ⚠ **Keyed by route key ALONE, the leg reached `vike_core`'s per-venue health gate as an unknown
/// venue, which reads `Healthy` on a miss.** Two accounts are different books, but the probe asks
/// whether THE EXCHANGE'S FEED is mid-gap — one fact both passes read through — so a degraded
/// binance feed must suppress both binance legs.
pub(super) fn runnable_accounts_of(
    venue: &str,
    config: &RuntimeConfig,
    table: &crate::ArmingTable,
    live_events: &vike_execution::EventSender,
    live_venues: &mut HashSet<String>,
    reconcile_trigger: Option<std::sync::mpsc::Sender<String>>,
    extras: &mut AccountExtras,
) -> Result<Option<crate::EngineAndReconcile>, RuntimeError> {
    // ⚠ ONE row per ACCOUNT, derived from the MOUNTS in `config.core_config` (not passed in), the same
    // `runnable_accounts` set `refuse_unarmed_mount_accounts` judged; `refuse_unarmed_live_venues`
    // then fails the node if what armed is not a subset of what the root locked.
    let account_symbols = account_symbols_for(
        &runnable_accounts(&config.core_config),
        venue,
        &config.policy.accounts,
    );
    let default_symbol = vike_config::account_arming::account_symbol(
        Some(&config.policy.accounts),
        venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
    );
    let mut env = crate::AdapterEnv::new(&*config.backend, table, live_events, live_venues);
    env.reconcile_enabled = config.reconcile_enabled;
    env.reconcile_trigger = reconcile_trigger;
    env.properties_rec = config.properties_rec.clone();
    env.risk_profile = config.risk_profile.as_ref();
    env.policy = Some(&config.policy);
    let crate::EngineAccounts { running, not_running } = crate::make_engine_accounts(
        &mut env,
        venue,
        &account_symbols,
        &declared_legs_for(&config.core_config, venue, default_symbol),
    )
    .map_err(RuntimeError::RiskBudget)?;
    extras.not_running.extend(not_running);
    // The default account is first when it runs (that function's order contract).
    let mut running = running.into_iter().peekable();
    let default = running.next_if(|(label, _)| label.is_default()).map(|(_, engine)| engine);
    for (label, (extra_engine, extra_reconcile)) in running {
        let route_key = extra_engine.route_key.clone();
        // ⚠ The engine's OWN symbol: the default account's would name another book's market.
        let account_symbol = extra_engine.symbol.clone();
        tracing::warn!(
            venue,
            symbol = %account_symbol,
            account = %label,
            route_key = %route_key,
            "mounting {venue} account `{label}` — its fills, positions and reconcile are its own \
             book, separate from every other {venue} account's"
        );
        extras.engines.push((crate::engine_seed(&extra_engine, config.seed_cash), extra_engine));
        if let Some(rc) = extra_reconcile {
            // BOTH facts: the canonical venue id (health gate, notes) and THIS account's route key
            // (which of the venue's engines the reports belong to).
            extras.reconcile.push(crate::ReconcileLeg::account(venue, route_key, rc));
        }
    }
    Ok(default)
}
