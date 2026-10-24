//! The start entry points: `make_engine`, its per-leg and per-account forms, and the fan-out.

use vike_model::accounts::account_keys::AccountLabel;

#[cfg(doc)]
use crate::{AdapterPolicy, account_symbols_for, assemble_runtime, symbol_grid};
use crate::{
    AdapterStartQuery, BackendError, EngineError, NotRunning, Withhold, account_event_sender,
    account_route_key, account_start_leverage, live_twin_must_hold_its_leverage,
    live_twin_not_started, paper_engine, report_halt_admit, report_not_running,
    report_table_decision, report_unaddressable_accounts, shared_book_ceiling_note,
    shared_books_for, symbol_for_account, venue_account_arming,
};

use assemble::{assemble_engine, parts_from_bind};
use vike_config::account_arming::account_tier;

mod assemble;
mod env;

pub use assemble::unanchorable_mount;
pub use env::{AdapterEnv, EngineAndRecon};

/// One execution engine for `venue`/`symbol` on the venue's DEFAULT account, no declared legs
/// ([`make_engine_with_legs`] with an empty slice). The client is type-erased as
/// `Box<dyn ExecutionClient>` so the cross-venue core drives every venue together; the reconcile
/// client is `Some` when a live venue's bridge start built one (e.g.
/// `crates/bridges/binance/src/adapter.rs`'s `BinanceVenueAdapter`), `None` for a `paper` account. The
/// caller of [`assemble_runtime`] spawns the reconcile driver from `AssembledRuntime::recon_clients`, gated on
/// `vike_tradehub::reconcile_config::reconcile_gate` (ON by default for a live start). The ordered
/// steps are the crate doc's.
///
/// ⚠ **An account runs at the tier its active row states, or it does not run**
/// (`docs/decisions/0156-an-account-runs-at-its-tier-or-not-at-all-and-accounttier-is-the-one-tier-type.md`):
/// a `paper` row gets the paper simulator, a `demo`/`live` row a venue client armed at exactly that
/// tier, and every other outcome is `Err(EngineError::NotRunning)` with its `ArmingBlock` — no
/// engine, and never a paper engine in its place.
///
/// LIVE GATE: each bridge decides in its own `resolve` and `start`, from `AdapterInputs::secrets` and
/// the account's tier (`AdapterInputs::tier`): binance/bybit/okx read `{VENUE}_DEMO_*`
/// for a `demo` account and `{VENUE}_LIVE_*` for a `live` one (decision 0095), deribit its demo
/// keys for a `demo` account; absent creds → NOT RUNNING, and so is a `live` account whose
/// bridge could reach only demo (the no-downgrade rule, `crate::contract`'s
/// `resolution_to_arming`). Binance routes spot for `BTCUSDT`, the USDⓈ-M perp for `BTCUSDT.P`.
/// ⚠ Aster is the REAL-MONEY exception: no `{VENUE}_DEMO_*` shape — `AsterVenueAdapter`
/// (`crates/bridges/aster/src/adapter.rs`) reads `ASTER_LIVE_*` for a `live` account and spawns a
/// LIVE MAINNET client. ⚠ Polymarket is the SECOND, with no
/// testnet at all: it needs `flags.poly_exec` ON TOP of its credentials, else it does not run. A
/// live start joins `env.live_venues` (the DOM's `● LIVE` badge, the LIVE window title). The PAPER
/// client a `paper` account runs fills against the venue's live 1m bars via `on_bar` (so its bar
/// feed must be subscribed — see `ensure_depth`); a LIVE client ignores `on_bar`.
///
/// # The [`AdapterEnv`] fields it reads
///
/// `env.recon_enabled` is the GLOBAL reconcile verdict (`crate::RuntimeConfig::recon_enabled`, from
/// `reconcile_gate`), SEPARATE from `env.recon_trigger` because `recon_trigger.is_some()` is the
/// WRONG gate: only binance/bybit/okx/hyperliquid take the recon trigger (a reconnect fires it), so
/// reading it as the gate would silently stop reconciling six live venues. It gates the venues
/// whose `ReconClient` does a BLOCKING, AUTHENTICATED handshake at start — deribit (an authed
/// order-WS, via `AdapterRequest::recon_enabled`), ctrader (protobuf/TLS OAuth), ig (`IgSession`
/// login), oanda (Bearer + `/summary`) and, behind `ibkr`, ibkr (cpapi `tickle`/`secdef_search`):
/// `false` never CALLS the factory (`vike_bridge_core::venue_adapter::recon_if_enabled`), `true` is
/// byte-identical to the ungated path. NOT gated (no mount-time connection): the crypto-CEX and
/// aster factories (alpaca's `AlpacaVenueAdapter::start` ignores the flag); hyperliquid's recon
/// client is inseparable from its exec client.
///
/// ⚠ **Polymarket is gated by BOTH flags.** Its `POLY_RECONCILE=1` gate says whether the VENUE
/// wants a client, not whether this process starts a driver, so with `flags.poly_reconcile` on and
/// reconcile off it paid a blocking L1→EOA + `/auth/derive-api-key` round trip for a handle a
/// driver-less root dropped (`crates/bridges/polymarket/src/exec_plane/adapter.rs`'s
/// `poly_recon_wanted`).
///
/// `env.recon_trigger` reaches each venue whose declaration's `takes_recon_trigger` is `true`
/// (binance, bybit, okx, hyperliquid; deribit's is `false`) into its resync path. [`assemble_runtime`]
/// builds it ONCE, before any start (the `ReconDriver` does not exist yet — `spawn_recon`'s doc),
/// and each start gets a clone, so any reconnect fires the SAME driver's trigger. Inert on a paper venue.
///
/// `env.properties_rec` is the opt-in PIT-filter recorder
/// (`vike_store::open_properties_recorder`, `None` unless the root's `flags.record_properties` row is on):
/// the binance/bybit/okx/aster/deribit starts move a clone into `spawn_with_recorder`. The BINARY
/// builds it because the constructor names the `DataFusionHist` backend (`vike-store`, which this
/// crate must not name).
///
/// `env.risk_profile` is the OPERATOR's `[risk]` budget for every venue, merged onto the
/// venue-fetched grid AFTER every arm but BEFORE the `im_requirement` rescue (load-bearing order;
/// the merge site says why). `max_orders_per_window`/`window_ms` ALWAYS arm to a conservative
/// default (`arm_universal_defaults`), which `Some` overrides (`ProfileRisk::apply_to`); a profile
/// illegally setting a venue-owned grid field loses only those fields
/// (`ProfileRisk::apply_operator_budget_only`). `max_notional_per_order`/`max_total_exposure` have
/// no universal default: a LIVE start without both returns `Err(EngineError::MissingRiskBudget)`
/// ([`crate::require_live_risk_budget`]); paper and backtest may run unbounded.
///
/// The grid source the merge judges the profile under is the ACCOUNT's tier
/// (`vike_model::GridSource::for_tier`, decision 0156 item 7): a run profile states no tier, so a
/// profile that sets a venue-owned instrument field LOADS, and the start of every `demo`/`live`
/// account it reaches is REFUSED (`EngineError::VenueOwnedRiskFields`, before any credential is
/// read) while a `paper` account takes it.
///
/// `env.policy` is this MACHINE's hard ceilings — the `policy` rows in
/// `<project>/settings/db/vike.db`, loaded by the BINARY and projected onto [`AdapterPolicy`]; a
/// different authority from `risk_profile` (per-machine, admin-owned, no env or CLI layer).
///
/// ⚠ **`None` runs NO account.** An account trades at its `account` row's tier only while the row
/// is active ([`AdapterPolicy::accounts`], `vike_config::account_arming`'s `account_tier`); no table, no row or an
/// inactive row is NOT RUNNING, closing credential presence as the only gate (MEASURED on the CI box: a
/// one-venue run profile holding NINE live authenticated exec sessions). The refusal happens ABOVE
/// the credential read and announces itself per account (`report_table_decision`). Every OTHER
/// field keeps its compiled-in behaviour under `None`
/// (`AdapterPolicy::default()`): `market_slippage` binds in the hyperliquid start (no native market
/// order), `halt_admit` in `crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter` and is
/// REPORTED for every venue (`report_halt_admit`, then `report_halt_admit_armed`), the exposure
/// and sizing-equity ceilings narrow the limits in the assembly. ⚠ No field count is written here:
/// "exactly one field binds" went stale the first time a second did. `AdapterPolicy`'s module doc
/// names every `Policy` field it does NOT carry; an exhaustive destructure gates that projection.
pub fn make_engine(
    env: &mut AdapterEnv<'_>,
    venue: &str,
    symbol: &str,
) -> Result<EngineAndRecon, EngineError> {
    make_engine_with_legs(env, venue, symbol, &[])
}

/// [`make_engine`] for a start that trades MORE THAN ONE symbol on this venue: `declared_legs` are
/// the EXTRA symbols its `StrategyMount`s declared, and they change only
/// `vike_model::RiskLimits::grid_by_symbol` — the per-symbol PRICE/SIZE GRID the `RiskGate` rounds
/// each order onto. An EMPTY slice touches nothing (`declared_symbol_grids` returns an empty map
/// without calling the venue, and an empty `grid_by_symbol` is `skip_serializing_if`, so even
/// `vike_exec::state_hash` is unchanged), which is why [`make_engine`] keeps its shape and
/// delegates here.
///
/// ⚠ **Not every arm can honour a declared leg, and the ones that cannot SAY so.** A leg is gridded
/// only from a source the arm ALREADY holds — no start gains a blocking network round trip per
/// leg. [`symbol_grid`]'s module doc is the authority, `crates/vike-gatewayhub/src/trade/adapters/symbol_grid.rs`'s `declared_grid_source` the per-venue
/// declaration, and `symbol_grid::warn_ungridded_legs` the operator's line when a leg falls back to
/// the engine symbol's grid (degraded, never newly broken).
///
/// A leg naming `symbol`, a blank one and a repeat are ignored, so a caller may pass its raw list.
///
/// ⚠ **It starts the venue's DEFAULT account and only that one**; the per-account fan-out is
/// [`make_engine_accounts`]. `AccountLabel::Default` is not "no account": it is THE account a
/// single-account box has, which [`make_engine_for_account`] renders unchanged.
pub fn make_engine_with_legs(
    env: &mut AdapterEnv<'_>,
    venue: &str,
    symbol: &str,
    declared_legs: &[String],
) -> Result<EngineAndRecon, EngineError> {
    make_engine_for_account(env, venue, symbol, &AccountLabel::Default, declared_legs)
}

/// What [`make_engine_accounts`] started for ONE venue: the engines of the accounts that RUN, and
/// every account it knows of that venue that does NOT RUN, each with why (decision 0156).
#[derive(Default)]
pub struct AccountEngines {
    /// One engine per account that runs: the DEFAULT account first when it runs, then labelled
    /// accounts in label order.
    pub running: Vec<(AccountLabel, EngineAndRecon)>,
    /// Every account of the venue that does not run — no engine, and no paper engine in its place.
    pub not_running: Vec<NotRunning>,
}

/// **THE FAN-OUT: one engine per account of `venue` that RUNS** (decision 0156). An account runs at
/// the tier its active row states or not at all: a `paper` row gets the paper simulator, a
/// `demo`/`live` row an engine only when it ARMS at exactly that tier ([`accounts_to_run`], then
/// the start itself — a failed connect or a declined key still leaves it not running). Every other
/// account — the DEFAULT one included, which no longer starts a paper engine by itself — lands in
/// [`AccountEngines::not_running`] with its reason, said once at `ARMING:` (`report_not_running`,
/// `report_table_decision`).
///
/// **Result order is the contract**: the default account FIRST when it runs, then labelled ones in
/// label order. [`assemble_runtime`] binds a venue's default engine at that venue's engine rank and
/// appends the rest to its `extra` list, so one account per venue yields the same engine vector as
/// a single-account start.
///
/// # ⚠ Sharing an instrument is NOT refused
///
/// Two accounts on one instrument is an ordinary spread: two wallets, separate positions
/// (`vike_config::venue_accounts`' module doc). The real hazard — two accounts resolving to ONE
/// venue BOOK — is **reported, never refused**: one `warn!` per pair naming the venue, both labels
/// and the book, and both engines mount (`docs/decisions/0013-degrade-vs-refuse.md`). Where
/// `book_identity` cannot determine the book offline, nothing is said.
///
/// # ⚠ Each account is started on ITS OWN symbol
///
/// `account_symbols` has one row per account — the DEFAULT on the venue's wired symbol, a labelled
/// one on the symbol of the strategy mount that NAMED it ([`account_symbols_for`]). One shared
/// symbol made a labelled account unaddressable OUTBOUND (mounted on a symbol its strategy had not
/// chosen). A caller with no labelled start passes `[(AccountLabel::Default, symbol)]`; two
/// accounts on one symbol is legal.
pub fn make_engine_accounts(
    env: &mut AdapterEnv<'_>,
    venue: &str,
    account_symbols: &[(AccountLabel, String)],
    declared_legs: &[String],
) -> Result<AccountEngines, EngineError> {
    let (table, policy) = (env.table, env.policy);
    // THE UNADDRESSABLE-ACCOUNT REPORT, once per process (`Once`-latched), BEFORE anything starts:
    // accounts this box names on a venue whose arm addresses only one. Said from the fan-out, not
    // `make_engine_for_account`, because such an account never reaches it (`accounts_to_run`
    // drops it — that IS the refusal) and its venue may have no
    // `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` engine row: a line from its own start is never read.
    // ⚠ It names NOTHING on any roster box today (every bridge declares `addresses_accounts`); the
    // call stays because `just new-venue` scaffolds `addresses_accounts: false` in a bridge's
    // `mount.rs` until its `resolve` and `start` read the NAMED account's keys.
    report_unaddressable_accounts(table, policy);
    // ⚠ The WHOLE policy: every account's tier, and a dukascopy account's keys, resolve out of
    // `AdapterPolicy::accounts`, the same snapshot the loop's `make_engine_for_account` calls read,
    // so these rows describe exactly what starts.
    let rows = venue_account_arming(table, venue, policy);
    // THE SHARED-BOOK WARNING, before anything starts — then BOTH accounts start. One line per
    // PAIR: `SharedBook::why` names both labels and the book, actionable in seconds.
    // ⚠ `warn!`, and the start CONTINUES: the operator wrote both credential sets under explicit
    // labels, and refusing would stop an account over a configuration they may have meant
    // (`docs/decisions/0013-degrade-vs-refuse.md`).
    // ⚠ DECLARED BLIND SPOT, measured: deleting this loop is caught by NO test in this crate.
    // Gated are the CONTENT (`book_identity`'s table test;
    // `crates/vike-gatewayhub/tests/trade_roster/shared_book_report.rs` drives `shared_books_for`) and the EFFECT
    // (`accounts_to_run`: `a_shared_book_removes_no_account_from_the_mount_set`, the mutation
    // that matters). The EMISSION is unreachable: two ACTIVE accounts need a real live arm.
    // ⚠ The ACCOUNT-aggregate ceiling MULTIPLIES on this shape: `max_account_exposure` is armed per
    // ENGINE, so two engines over one ledger apply it twice (the N×-looser defect, wearing the
    // account label). Said HERE, the one moment both facts are known; empty when it is off.
    let ceiling_note = shared_book_ceiling_note(policy.and_then(|p| p.max_account_exposure));
    // ⚠ CAPPED: pairs are QUADRATIC (N accounts on one wallet = N(N-1)/2, 1,225 lines at the fifty
    // this design is scoped for), and the fact worth knowing — fifty are ONE wallet — sits in no
    // single line. The split is arithmetic in `vike-config`, where it is testable.
    let report = vike_config::shared_book_report(
        shared_books_for(table, &rows),
        vike_config::SHARED_BOOK_REPORT_CAP,
    );
    for shared in &report.shown {
        tracing::warn!(
            venue,
            book = %shared.book,
            first = %shared.first,
            second = %shared.second,
            "TWO {venue} ACCOUNTS SHARE ONE BOOK: {}. Both are being mounted — this is a report, \
             not a refusal.{ceiling_note}",
            shared.why()
        );
    }
    if report.suppressed > 0 {
        // The aggregate line: how many DISTINCT accounts sit on each book. `ceiling_note` rides
        // here too — the per-engine ceiling applies once per account on the book.
        let books = report
            .books
            .iter()
            .map(|(book, accounts)| format!("`{book}` ({accounts} accounts)"))
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            venue,
            suppressed = report.suppressed,
            "…and {} more {venue} account PAIRS share a book, not listed one by one. What the \
             pairs above cannot say: {books}. Each of those accounts is a separate engine over ONE \
             venue position ledger. If that is not what you meant, the credential sets for that \
             book name the same account more than once.{ceiling_note}",
            report.suppressed
        );
    }
    // THE ACCOUNTS THE LOOP NEVER STARTS: one that does not run for want of a usable key set never
    // reaches its bridge's `start`, where "a live-tier key set is stored and unused" is said. The
    // bridge speaks for each here — the DEFAULT account included, which no longer starts a paper
    // engine to hear it out — once per start, over rows already computed; nothing it does changes
    // what starts.
    env.backend.report_not_running(venue, &rows);
    let mut out = AccountEngines::default();
    if rows.is_empty() {
        // A venue `vike_model::VENUES` does not carry (a test or sim id): one account, so the
        // single start — which runs only what an account row states for it.
        let label = AccountLabel::Default;
        match make_engine_for_account(
            env,
            venue,
            symbol_for_account(account_symbols, &label),
            &label,
            declared_legs,
        ) {
            Ok(engine) => out.running.push((label, engine)),
            Err(EngineError::NotRunning(not_running)) => out.not_running.push(not_running),
            Err(refusal) => return Err(refusal),
        }
        return Ok(out);
    }
    // WHICH accounts are started is decided ONCE, by a function that cannot see a book; there is no
    // second filter here. Every other row is NOT RUNNING with its own block, said once.
    let mounted = accounts_to_run(&rows);
    for row in &rows {
        if !mounted.contains(&row.label) {
            let not_running = NotRunning {
                venue: venue.to_string(),
                account: row.label.clone(),
                tier: row.tier,
                block: row.block,
            };
            report_account_not_running(&not_running, policy);
            out.not_running.push(not_running);
            continue;
        }
        // …each on ITS OWN symbol, so `ExecutionEngine::accepts_symbol` answers per engine. `env`'s
        // reconnect trigger and properties recorder are CLONED into each account's start. A start
        // that does not come up (a failed connect, a declined key) is NOT RUNNING too.
        match make_engine_for_account(
            env,
            venue,
            symbol_for_account(account_symbols, &row.label),
            &row.label,
            declared_legs,
        ) {
            Ok(engine) => out.running.push((row.label.clone(), engine)),
            Err(EngineError::NotRunning(not_running)) => out.not_running.push(not_running),
            Err(refusal) => return Err(refusal),
        }
    }
    Ok(out)
}

/// Say, once, why an account [`accounts_to_run`] left out does not run: the account table's
/// causes through [`report_table_decision`] (its levels: a conflict is an error, keys that would
/// have armed it a warning) — for the DEFAULT account and for a labelled TWO-TIER conflict, the two
/// it has always spoken for — and every other cause through [`report_not_running`].
fn report_account_not_running(not_running: &NotRunning, policy: Option<&crate::AdapterPolicy>) {
    let NotRunning { venue, account, tier, block } = not_running;
    if tier.is_some() {
        report_not_running(not_running, policy);
    } else if account.is_default() || *block == vike_config::ArmingBlock::TierConflict {
        report_table_decision(venue, account, *block, policy);
    }
}

/// **WHICH accounts of a venue get an ENGINE** — [`make_engine_accounts`]' start decision, lifted
/// out of its loop so a test can reach it: exactly the accounts whose arming row RUNS
/// (`vike_config::VenueArming::runs` — a `paper` row on the simulator, a `demo`/`live` row the
/// probe arms at exactly that tier). Since decision 0156 that is the DEFAULT account's rule too: it
/// is no longer started on paper by itself. Row order is kept, so the default stays first when it
/// runs (load-bearing, see [`make_engine_accounts`]).
///
/// # ⚠ A shared BOOK must never remove an account from this set
///
/// It is a WARNING and a mount, not a refusal (`docs/decisions/0013-degrade-vs-refuse.md`;
/// [`shared_books_for`] computes the report). Dropping every `SharedBook::second` from the start
/// loop — the operator's second account silently never started — was measured GREEN across this
/// crate and `vike-run` (then separate), because that loop needs two ACTIVE accounts and a real
/// socket. So the signature is the guard: `VenueArming` rows only (label, tier, mode, block —
/// **no address**), so a book-based refusal cannot be written here without widening the signature
/// to take `vars`, a visible change at a reviewed seam.
#[must_use]
pub fn accounts_to_run(rows: &[vike_config::VenueArming]) -> Vec<AccountLabel> {
    rows.iter().filter(|row| row.runs()).map(|row| row.label.clone()).collect()
}

/// [`make_engine_with_legs`] for ONE named ACCOUNT of the venue; that function delegates here at
/// [`AccountLabel::Default`], which keeps a single-account box byte-identical. `account` selects:
///
/// * the account's TIER: `vike_config::account_arming::account_tier`, the `account` rows of `(venue, label)` (the DEFAULT
///   account's are the rows with no label);
/// * the credentials the bridge reads: the ONE active `account` row of `(venue, label)` at the
///   account's tier, and its fields (`vike_secrets::account_directory::AccountDirectory::credentials_for`,
///   decision 0156 item 6);
/// * the per-account exposure ceiling (the account rows' `max_exposure`) on the limits;
/// * the engine's `route_key` and its `live_venues` entry,
///   [`vike_model::accounts::account_keys::AccountRef::route_key`] — the bare venue id for the
///   default account, so `vike_boot::live_lock`'s `LIVE-<route_key>.lock` sentinel does not move.
///
/// `Err(EngineError::NotRunning)` for an account that does not run at its row's tier (decision
/// 0156), said once at `ARMING:`; no engine and no paper engine in its place.
///
/// ⚠ It starts whatever it is asked to. **The shared-BOOK report is NOT made here**: it is a fact
/// about the SET of accounts, which [`make_engine_accounts`] (every composition root's entry
/// point) sees and this function does not.
pub fn make_engine_for_account(
    env: &mut AdapterEnv<'_>,
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
) -> Result<EngineAndRecon, EngineError> {
    // ⚠ NOT named `live_events`, and the name is the mechanism: the only `live_events` binding here
    // is the account-scoped lane below ([`account_event_sender`]), so deleting it fails to compile
    // (the `ContractCall` field) instead of silently reverting to the unscoped lane — a mutation no
    // test in this crate can observe, since reaching a venue arm needs credentials and a socket.
    let venue_events = env.live_events;
    let recon_enabled = env.recon_enabled;
    let (risk_profile, policy) = (env.risk_profile, env.policy);
    // ---- THE BOX-WIDE LEVERAGE CEILING: the ONE site `policy.max_leverage` is enforced at ----
    // It NARROWS the run profile's `[risk] max_leverage` (a `min`, never a raise, never a leverage
    // of its own: `ProfileRisk::narrowed_to_leverage_ceiling`) BEFORE anything reads the profile,
    // so every consumer sees the same number: the buying-power gate (`im = 1/max_leverage`, via
    // `merge_operator_budget`) on a live and a `paper`-tier engine alike, and the account leverage
    // each perp bridge posts (`AdapterRequest::risk_profile` -> `leverage_for`), which must never
    // disagree with the gate. No `policy.max_leverage` row (`None`) returns the profile unchanged,
    // field for field. Shadowed on purpose: nothing below may reach the un-narrowed profile.
    let narrowed_risk =
        risk_profile.map(|r| r.narrowed_to_leverage_ceiling(policy.and_then(|p| p.max_leverage)));
    let risk_profile = narrowed_risk.as_ref();
    // The halt-admit mode in force, its DEGRADE reported once, here, at start. Whether `verify`
    // ARMED is answered later (`report_halt_admit_armed` in `assemble_engine`): this runs before
    // credentials and cTrader's blocking handshake, either of which can leave the account not
    // running.
    let halt_admit = report_halt_admit(venue, policy);
    // ---- THE ACCOUNT'S TIER ----
    // ⚠ THE ONE SEAM, ABOVE the credential read on purpose: everything below (credentials, the
    // `SymbolProperties` pre-fetch, the pre-connect budget refusal, every arm's blocking
    // handshake) is work for an account the operator may not have armed, and discarding the client
    // afterwards still leaves an authenticated session on a real account (MEASURED on the CI box: a
    // one-venue run profile printed
    // `live_venues={hyperliquid,deribit,okx,bybit,alpaca,aster,binance,ig,oanda}`).
    // `env.policy == None` runs NOTHING: widening has to be typed. Every production start passes
    // `Some` (`crate::assemble_runtime` from `RuntimeConfig::policy`), so `None` is test callers — exactly
    // the population that must not silently arm. No row, an inactive row or two active non-paper
    // tiers all read NOT RUNNING, each with its own `block`; a `paper` tier runs the simulator.
    let tier = match account_tier(policy.map(|p| &p.accounts), venue, account) {
        Ok(tier) => tier,
        Err(block) => {
            report_table_decision(venue, account, block, policy);
            return Err(EngineError::NotRunning(NotRunning {
                venue: venue.to_string(),
                account: account.clone(),
                tier: None,
                block,
            }));
        }
    };
    // The ROUTING identity: the bare venue id for the default account, `venue#LABEL` otherwise.
    // Resolved ONCE, threaded into the engine and `live_venues`.
    let route_key = account_route_key(venue, account);
    // …and the EXEC-EVENT LANE scoped to it: every arm pushes account-tagged frames with no
    // bridge knowing accounts exist. [`account_event_sender`] says why it is unconditional and
    // inert for a default account.
    let live_events = &account_event_sender(venue_events, &route_key);
    if tier == vike_model::AccountTier::Paper {
        report_table_decision(venue, account, vike_config::ArmingBlock::PaperTier, policy);
        // ⚠ THE ROUTE KEY IS STAMPED ON THIS PATH TOO (a mutation test found it missing):
        // `ExecutionEngine::new` seeds `route_key = venue`, WRONG for a labelled account, and two
        // engines sharing one make the second unreachable
        // (`vike_core::CoreThread::engine_idx_for_route_key` returns the first match). A labelled
        // `paper` account reaches here, and the route-key assertion in
        // `crates/vike-tradehub/tests/account_fanout.rs` exercises `account_route_key` rather than
        // `ExecutionEngine::new`'s seed.
        let (mut engine, recon) =
            paper_engine(venue, symbol, account, declared_legs, risk_profile, policy);
        engine.route_key = route_key;
        return Ok((engine, recon));
    }
    // A `demo`/`live` account starts the venue's grid: a profile that sets it REFUSES the start
    // here, before any credential is read (decision 0156 item 7).
    crate::budget::refuse_venue_owned_fields(venue, risk_profile, tier)?;
    // The static fee schedule, evaluated ONCE: `resolve_fee_schedule` prefers a live rate over it.
    // KEYED BY LANE, not the bare venue: binance and aster start SPOT and USDⓈ-M PERP under one id
    // (a trailing `.P`), and binance's maker fee differs 5x between them — keyed by venue, every
    // `BTCUSDT.P` paper/backtest start filled at SPOT fees. `fee_lane` is the identity for every
    // other venue and every non-`.P` symbol.
    let static_default = vike_venues::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    // THE VENUE'S OWN HALF: the arming table says what the venue's adapter is and whether this
    // account is armed, and a start goes through the backend. A venue whose bridge this backend
    // does not carry, or one the table does not name (a test's planted id), runs no `demo`/`live`
    // account at all (docs/decisions/0096, 0156).
    let not_running = |block| NotRunning {
        venue: venue.to_string(),
        account: account.clone(),
        tier: Some(tier),
        block,
    };
    let parts = match env.table.facts_of(venue) {
        None => Err(EngineError::NotRunning(not_running(vike_config::ArmingBlock::NoLiveArm))),
        Some(facts) if !facts.adapter_present => {
            Err(EngineError::NotRunning(not_running(vike_config::ArmingBlock::FeatureAbsent)))
        }
        Some(facts) => 'start: {
            // THE ONE PROBE: this account's row of the snapshot, the startup preflight's stop laid
            // over it. The tradehub's own tier (`account_tier` above) must equal the row's: a
            // mismatch means the table and the policy are not one snapshot, and the account is NOT
            // RUNNING with the table's word — never silently armed.
            let Some(row) = env.table.row_of_account(venue, account) else {
                tracing::error!(
                    venue,
                    account = %account,
                    "{venue}: the arming table has no row for account `{account}`, although its \
                     tier reads `{tier}` — the table and the policy are not one snapshot. Refused: \
                     the account does NOT RUN."
                );
                break 'start Err(EngineError::NotRunning(not_running(
                    vike_config::ArmingBlock::AdapterFailed,
                )));
            };
            let verdict = crate::arming::with_the_preflight_stop(row, policy);
            if verdict.tier != Some(tier) {
                tracing::error!(
                    venue,
                    account = %account,
                    tier = tier.as_str(),
                    "{venue}: the arming table states tier {:?} for account `{account}`, but its \
                     account rows read `{tier}` — the table and the policy are not one snapshot. \
                     Refused: the account does NOT RUN.",
                    verdict.tier.map(vike_model::AccountTier::as_str)
                );
                break 'start Err(EngineError::NotRunning(not_running(verdict.block)));
            }
            // THE PRE-CONNECT BUDGET REFUSAL, for an account the table calls armed and before the
            // venue is asked to start it — the position it had in the old body, after the arming
            // probe's refusals. An armed-at-live row under a lower tier is the one armed answer the
            // start's tier interlock refuses before the budget ever applies, so the budget skips it.
            let armed = verdict.effective.is_some_and(|effective| {
                !(effective == vike_model::AccountTier::Live
                    && tier != vike_model::AccountTier::Live)
            });
            if armed {
                crate::arming::require_live_risk_budget(
                    venue,
                    &risk_profile.map(vike_model::ProfileRisk::to_risk_limits).unwrap_or_default(),
                    risk_profile.is_some(),
                )?;
            }
            let withhold = policy
                .is_some_and(|p| p.preflight_failed.contains(venue))
                .then_some(Withhold::PreflightFailed);
            // Decision 0182: the whole-number figure a venue that POSTS a leverage is asked to hold for
            // this account; `None` (every other start) changes nothing on the wire.
            let account_leverage =
                account_start_leverage(policy, venue, account, tier, risk_profile);
            // ⚠ A MAINNET LANE TWIN (binance's `PERP` at `live`) exists only because its row states a
            // leverage, and its start WRITES that figure to the real account. With no figure to ask
            // (a planted fraction the table refuses, a ceiling that left nothing) the bridge would write
            // its default instead: the twin is refused here, before the venue is asked anything.
            if account_leverage.is_none()
                && withhold.is_none()
                && live_twin_must_hold_its_leverage(policy, venue, account, tier)
            {
                tracing::error!(
                    venue,
                    account = %account,
                    "{}",
                    live_twin_not_started(
                        venue,
                        &route_key,
                        "the account's stated leverage cannot be asked of the venue (the table \
                         refuses the figure, or no whole number of at least 1 is left under \
                         `policy.max_leverage` and the run profile's `max_leverage`)"
                    )
                );
                break 'start Err(EngineError::NotRunning(not_running(
                    vike_config::ArmingBlock::AdapterFailed,
                )));
            }
            let started = env.backend.start_adapter(
                AdapterStartQuery {
                    venue,
                    account,
                    tier,
                    symbol,
                    declared_legs,
                    recon_enabled,
                    risk_profile,
                    market_slippage: policy.and_then(|p| p.market_slippage),
                    halt_admit,
                    withhold,
                    account_leverage,
                    route_key: &route_key,
                    // CLONED per start; the original stays in `env` for the caller's next start.
                    properties_rec: env.properties_rec.clone(),
                },
                live_events,
                // CLONED per start; the original stays in `env` for the caller's next start.
                env.recon_trigger.clone(),
            );
            match started {
                Ok(bind) => Ok(parts_from_bind(
                    symbol,
                    declared_legs,
                    bind,
                    facts.grid_source,
                    static_default,
                )),
                Err(BackendError::NotRunning(block)) => {
                    Err(EngineError::NotRunning(not_running(block)))
                }
                // The gatewayhub did not answer: the account stops with its own block (decision 0164
                // D7), and never on a paper stand-in (decision 0156).
                Err(BackendError::Unavailable(why)) => {
                    tracing::error!(venue, account = %account, "{venue}: {why}");
                    Err(EngineError::NotRunning(not_running(
                        vike_config::ArmingBlock::GatewayUnavailable,
                    )))
                }
            }
        }
    };
    let parts = match parts {
        Ok(parts) => parts,
        Err(EngineError::NotRunning(not_running)) => {
            report_not_running(&not_running, policy);
            return Err(EngineError::NotRunning(not_running));
        }
        Err(refusal) => return Err(refusal),
    };
    assemble_engine(
        env.backend,
        venue,
        symbol,
        account,
        declared_legs,
        parts,
        env.live_venues,
        route_key,
        halt_admit,
        risk_profile,
        policy,
    )
}
