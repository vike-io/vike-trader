//! The mount entry points: `make_engine`, its per-leg and per-account forms, and the fan-out.

use vike_model::accounts::account_keys::AccountLabel;

use crate::{
    MountError, VenueRow, account_event_sender, account_route_key, account_tier, contract,
    paper_engine, report_capped_to_paper, report_halt_admit, report_unaddressable_accounts, row_of,
    shared_book_ceiling_note, shared_books_for, symbol_for_account, tier_permits_live,
    venue_account_arming,
};
#[cfg(doc)]
use crate::{MountPolicy, account_symbols_for, build_node, declared_grid_source, symbol_grid};

use assemble::assemble_engine;

mod assemble;
mod env;

pub(crate) use assemble::MountParts;
pub use env::{EngineAndRecon, MountEnv};

/// One execution engine for `venue`/`symbol` on the venue's DEFAULT account, no declared legs
/// ([`make_engine_with_legs`] with an empty slice). The client is type-erased as
/// `Box<dyn ExecutionClient>` so the cross-venue core drives every venue together; the reconcile
/// client is `Some` when a live venue's bridge mount built one (e.g.
/// `crates/bridges/binance/src/mount.rs`'s `BinanceVenueMount`), `None` for paper. The caller of
/// [`build_node`] spawns the reconcile driver from `Node::recon_clients`, gated on
/// `vike_tradehub::reconcile_config::reconcile_gate` (ON by default for a live mount). The ordered
/// steps are the crate doc's.
///
/// LIVE GATE: each bridge decides in its own `resolve` and `mount`, from `MountInputs::secrets` and
/// the account's tier (`MountInputs::live_permitted`): binance/bybit/okx read `{VENUE}_DEMO_*`
/// for a `demo` account and `{VENUE}_LIVE_*` for a `live` one (decision 0095), deribit its demo
/// keys for a `demo` account; absent creds → the PAPER client, and so is a `live` account whose
/// bridge could reach only demo (the no-downgrade rule, `crate::contract`'s
/// `resolution_to_arming`). Binance routes spot for `BTCUSDT`, the USDⓈ-M perp for `BTCUSDT.P`.
/// ⚠ Aster is the REAL-MONEY exception: no `{VENUE}_DEMO_*` shape — `AsterVenueMount`
/// (`crates/bridges/aster/src/mount.rs`) reads `ASTER_LIVE_*` for a `live` account and spawns a
/// LIVE MAINNET client. ⚠ Polymarket is the SECOND, with no
/// testnet at all: it needs `flags.poly_exec` ON TOP of its credentials, else it stays paper. A
/// live mount joins `env.live_venues` (the DOM's `● LIVE` badge, the LIVE window title). The PAPER
/// client fills against the venue's live 1m bars via `on_bar` (so its bar feed must be subscribed —
/// see `ensure_depth`); a LIVE client ignores `on_bar`.
///
/// # The [`MountEnv`] fields it reads
///
/// `env.recon_enabled` is the GLOBAL reconcile verdict (`crate::NodeConfig::recon_enabled`, from
/// `reconcile_gate`), SEPARATE from `env.recon_trigger` because `recon_trigger.is_some()` is the
/// WRONG gate: the trigger is a RECONNECT poke wired for binance/bybit/okx/hyperliquid only, so
/// reading it as the gate would silently stop reconciling six live venues. It gates the venues
/// whose `ReconClient` does a BLOCKING, AUTHENTICATED handshake at mount — deribit (an authed
/// order-WS, via `MountRequest::recon_enabled`), ctrader (protobuf/TLS OAuth), ig (`IgSession`
/// login), oanda (Bearer + `/summary`) and, behind `ibkr`, ibkr (cpapi `tickle`/`secdef_search`):
/// `false` never CALLS the factory (`vike_bridge_core::venue_mount::recon_if_enabled`), `true` is
/// byte-identical to the ungated path. NOT gated (no mount-time connection): the crypto-CEX and
/// aster factories (alpaca's `AlpacaVenueMount::mount` ignores the flag); hyperliquid's recon
/// client is inseparable from its exec client.
///
/// ⚠ **Polymarket is gated by BOTH flags.** Its `POLY_RECONCILE=1` gate says whether the VENUE
/// wants a client, not whether this process mounts a driver, so with `flags.poly_reconcile` on and
/// reconcile off it paid a blocking L1→EOA + `/auth/derive-api-key` round trip for a handle a
/// driver-less root dropped (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s
/// `poly_recon_wanted`).
///
/// `env.recon_trigger` reaches each venue whose declaration's `takes_recon_trigger` is `true`
/// (binance, bybit, okx, hyperliquid; deribit's is `false`) into its resync path. [`build_node`]
/// builds it ONCE, before any mount (the `ReconDriver` does not exist yet — `spawn_recon`'s doc),
/// and each mount gets a clone, so any reconnect pokes the SAME driver. Inert on a paper venue.
///
/// `env.properties_rec` is the opt-in PIT-filter recorder
/// (`vike_data::PropertiesRecorder::open`, `None` unless the root's `flags.record_properties` row is on):
/// the binance/bybit/okx/aster/deribit mounts move a clone into `spawn_with_recorder`. The BINARY
/// builds it because the constructor names the `DataFusionHist` backend (vike-data's
/// `hist-datafusion`, which this crate does not enable).
///
/// `env.risk_profile` is the OPERATOR's `[risk]` budget for every venue, merged onto the
/// venue-fetched grid AFTER every arm but BEFORE the `im_requirement` rescue (load-bearing order;
/// the merge site says why). `max_orders_per_window`/`window_ms` ALWAYS arm to a conservative
/// default (`arm_universal_defaults`), which `Some` overrides (`ProfileRisk::apply_to`); a profile
/// illegally setting a venue-owned grid field loses only those fields
/// (`ProfileRisk::apply_operator_budget_only`). `max_notional_per_order`/`max_total_exposure` have
/// no universal default: a LIVE mount without both returns `Err(MountError::MissingRiskBudget)`
/// ([`crate::require_live_risk_budget`]); paper and backtest may run unbounded.
///
/// **Caller contract:** this function cannot see a `RunProfile`'s `mode`, so a caller resolving
/// one MUST gate it through `vike_core::RunProfile::risk_for_live_venue_mount` (`Err` unless
/// `mode == Mode::Live`): a backtest/paper profile may LEGALLY set venue-owned instrument fields,
/// which would fail every arm's merge under `GridSource::VenueFetched` — fail loud at resolution,
/// not degrade quietly here.
///
/// `env.policy` is this MACHINE's hard ceilings — the `policy` rows in
/// `<project>/settings/db/vike.db`, loaded by the BINARY and projected onto [`MountPolicy`]; a
/// different authority from `risk_profile` (per-machine, admin-owned, no env or CLI layer).
///
/// ⚠ **`None` mounts every venue PAPER.** An account trades at its `account` row's tier only while
/// the row is active ([`MountPolicy::accounts`], `crate::arming`'s `account_tier`); no table, no
/// row, an inactive row or a `paper` tier is PAPER, closing credential presence as the only gate
/// (MEASURED on the CI box: a one-venue run profile holding NINE live authenticated exec sessions). The
/// refusal happens ABOVE the credential read and announces itself per account
/// (`report_capped_to_paper`). Every OTHER field keeps its compiled-in behaviour under `None`
/// (`MountPolicy::default()`): `market_slippage` binds in the hyperliquid mount (no native market
/// order), `halt_admit` in `crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount` and is
/// REPORTED for every venue (`report_halt_admit`, then `report_halt_admit_armed`), the exposure
/// and sizing-equity ceilings narrow the limits in the assembly. ⚠ No field count is written here:
/// "exactly one field binds" went stale the first time a second did. `MountPolicy`'s module doc
/// names every `Policy` field it does NOT carry; an exhaustive destructure gates that projection.
pub fn make_engine(
    env: &mut MountEnv<'_>,
    venue: &str,
    symbol: &str,
) -> Result<EngineAndRecon, MountError> {
    make_engine_with_legs(env, venue, symbol, &[])
}

/// [`make_engine`] for a mount that trades MORE THAN ONE symbol on this venue: `declared_legs` are
/// the EXTRA symbols its `StrategyMount`s declared, and they change only
/// `vike_model::RiskLimits::grid_by_symbol` — the per-symbol PRICE/SIZE GRID the `RiskGate` rounds
/// each order onto. An EMPTY slice touches nothing (`declared_symbol_grids` returns an empty map
/// without calling the venue, and an empty `grid_by_symbol` is `skip_serializing_if`, so even
/// `vike_exec::state_hash` is unchanged), which is why [`make_engine`] keeps its shape and
/// delegates here.
///
/// ⚠ **Not every arm can honour a declared leg, and the ones that cannot SAY so.** A leg is gridded
/// only from a source the arm ALREADY holds — no mount gains a blocking network round trip per
/// leg. [`symbol_grid`]'s module doc is the authority, [`declared_grid_source`] the per-venue
/// declaration, and `symbol_grid::warn_ungridded_legs` the operator's line when a leg falls back to
/// the mounted symbol's grid (degraded, never newly broken).
///
/// A leg naming `symbol`, a blank one and a repeat are ignored, so a caller may pass its raw list.
///
/// ⚠ **It mounts the venue's DEFAULT account and only that one**; the per-account fan-out is
/// [`make_engine_accounts`]. `AccountLabel::Default` is not "no account": it is THE account a
/// single-account box has, which [`make_engine_for_account`] renders unchanged.
pub fn make_engine_with_legs(
    env: &mut MountEnv<'_>,
    venue: &str,
    symbol: &str,
    declared_legs: &[String],
) -> Result<EngineAndRecon, MountError> {
    make_engine_for_account(env, venue, symbol, &AccountLabel::Default, declared_legs)
}

/// **THE FAN-OUT: one engine per ACTIVE account of `venue`.** The DEFAULT account is always
/// mounted, at whatever tier it resolves to (paper included: the engine a paper box trades on). A
/// LABELLED account mounts only when it ARMS — an active `account` row states a tier above paper
/// AND its credentials load ([`accounts_to_mount`]); otherwise it gets NO engine, since a paper
/// engine nobody armed is a second local book `vike_core`'s mount resolution would bind a strategy
/// to.
///
/// **Result order is the contract**: the default account FIRST, then labelled ones in label order.
/// [`build_node`] binds `[0]` as the venue's primary engine and appends the rest to its `extra`
/// list, so one account per venue yields the same engine vector as a single-account mount.
///
/// # ⚠ Sharing an instrument is NOT refused
///
/// Two accounts on one instrument is an ordinary spread: two wallets, separate positions
/// (`vike_config::venue_accounts`' module doc). The real hazard — two accounts resolving to ONE
/// venue BOOK — is **reported, never refused**: one `warn!` per pair naming the venue, both labels
/// and the book, and both engines mount (`docs/decisions/0013-degrade-vs-refuse.md`). Where
/// `book_identity` cannot determine the book offline, nothing is said.
///
/// # ⚠ Each account is mounted on ITS OWN symbol
///
/// `account_symbols` has one row per account — the DEFAULT on the venue's wired symbol, a labelled
/// one on the symbol of the strategy mount that NAMED it ([`account_symbols_for`]). One shared
/// symbol made a labelled account unaddressable OUTBOUND (mounted on a symbol its strategy had not
/// chosen). A caller with no labelled mount passes `[(AccountLabel::Default, symbol)]`; two
/// accounts on one symbol is legal.
pub fn make_engine_accounts(
    env: &mut MountEnv<'_>,
    venue: &str,
    account_symbols: &[(AccountLabel, String)],
    declared_legs: &[String],
) -> Result<Vec<(AccountLabel, EngineAndRecon)>, MountError> {
    let (registry, vars, policy) = (env.registry, env.vars, env.policy);
    // THE UNADDRESSABLE-ACCOUNT REPORT, once per process (`Once`-latched), BEFORE anything mounts:
    // accounts this box names on a venue whose arm addresses only one. Said from the fan-out, not
    // `make_engine_for_account`, because such an account never reaches it (`accounts_to_mount`
    // drops it — that IS the refusal) and its venue may have no
    // `vike_tradehub::wired_markets::WIRED_MARKETS` row: a line from its own mount is never read.
    // ⚠ It names NOTHING on any roster box today (every bridge declares `addresses_accounts`); the
    // call stays because `just new-venue` scaffolds `addresses_accounts: false` in a bridge's
    // `mount.rs` until its `resolve` and `mount` read the NAMED account's keys.
    report_unaddressable_accounts(registry, vars, policy);
    // ⚠ The WHOLE policy: every account's tier, and a dukascopy account's keys, resolve out of
    // `MountPolicy::accounts`, the same snapshot the loop's `make_engine_for_account` calls read,
    // so these rows describe exactly what mounts.
    let rows = venue_account_arming(registry, venue, vars, policy);
    // TWO ACTIVE TIERS FOR ONE ACCOUNT, said LOUDLY for the labelled accounts: they resolve paper,
    // so `accounts_to_mount` drops them and `make_engine_for_account` (which speaks for the
    // DEFAULT account, `report_capped_to_paper`) never sees them. A stop, not a pick: which of a
    // demo and a mainnet key set to trade is the operator's to say.
    for row in rows.iter().filter(|r| !r.is_default_account()) {
        if row.block == vike_config::ArmingBlock::TierConflict {
            tracing::error!(venue, account = %row.label, "{}", row.why());
        }
    }
    // THE SHARED-BOOK WARNING, before anything mounts — then BOTH accounts mount. One line per
    // PAIR: `SharedBook::why` names both labels and the book, actionable in seconds.
    // ⚠ `warn!`, and the mount CONTINUES: the operator wrote both credential sets under explicit
    // labels, and refusing would strand a venue on paper over a configuration they may have meant
    // (`docs/decisions/0013-degrade-vs-refuse.md`).
    // ⚠ DECLARED BLIND SPOT, measured: deleting this loop is caught by NO test in this crate.
    // Gated are the CONTENT (`book_identity`'s table test;
    // `crates/vike-tradehub/tests/shared_book_report.rs` drives `shared_books_for`) and the EFFECT
    // (`accounts_to_mount`: `a_shared_book_removes_no_account_from_the_mount_set`, the mutation
    // that matters). The EMISSION is unreachable: two ACTIVE accounts need a real live arm.
    // ⚠ The ACCOUNT-aggregate ceiling MULTIPLIES on this shape: `max_account_exposure` is armed per
    // ENGINE, so two engines over one ledger apply it twice (the N×-looser defect, wearing the
    // account label). Said HERE, the one moment both facts are known; empty when it is off.
    let ceiling_note = shared_book_ceiling_note(policy.and_then(|p| p.max_account_exposure));
    // ⚠ CAPPED: pairs are QUADRATIC (N accounts on one wallet = N(N-1)/2, 1,225 lines at the fifty
    // this design is scoped for), and the fact worth knowing — fifty are ONE wallet — sits in no
    // single line. The split is arithmetic in `vike-config`, where it is testable.
    let report = vike_config::shared_book_report(
        shared_books_for(registry, venue, &rows, vars, policy),
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
    // THE LABELLED ACCOUNTS THE LOOP NEVER MOUNTS: one that resolved paper for want of a usable key
    // set never reaches its bridge's `mount`, where the default account's "a live-tier key set is
    // stored and unused" line is said. The bridge speaks for each here, once per start, over rows
    // already computed; nothing it does changes what mounts.
    contract::report_unmounted_accounts(registry, venue, &rows, vars, policy);
    if rows.is_empty() {
        // A venue `vike_model::VENUES` does not carry (a test or sim id): one account, no policy
        // row, so the single mount.
        let engine = make_engine_for_account(
            env,
            venue,
            symbol_for_account(account_symbols, &AccountLabel::Default),
            &AccountLabel::Default,
            declared_legs,
        )?;
        return Ok(vec![(AccountLabel::Default, engine)]);
    }
    // WHICH accounts get an engine is decided ONCE, by a function that cannot see a book; there is
    // no second filter here.
    let mut out = Vec::with_capacity(1);
    for label in accounts_to_mount(&rows) {
        // …each on ITS OWN symbol, so `ExecutionEngine::accepts_symbol` answers per engine. `env`'s
        // reconnect trigger and properties recorder are CLONED into each account's mount.
        let engine = make_engine_for_account(
            env,
            venue,
            symbol_for_account(account_symbols, &label),
            &label,
            declared_legs,
        )?;
        out.push((label, engine));
    }
    Ok(out)
}

/// **WHICH accounts of a venue get an ENGINE** — [`make_engine_accounts`]' mount decision, lifted
/// out of its loop so a test can reach it. The DEFAULT account always (callers bind it at `[0]`); a
/// LABELLED account exactly when it ARMED (`vike_config::VenueArming::effective` above `Paper`):
/// one that resolved paper has no credentials, or no active account row stating a tier above paper.
/// Row order is kept, so the default stays first (load-bearing, see [`make_engine_accounts`]).
///
/// # ⚠ A shared BOOK must never remove an account from this set
///
/// It is a WARNING and a mount, not a refusal (`docs/decisions/0013-degrade-vs-refuse.md`;
/// [`shared_books_for`] computes the report). Dropping every `SharedBook::second` from the mount
/// loop — the operator's second account silently never mounted — was measured GREEN across this
/// crate and `vike-run` (then separate), because that loop needs two ACTIVE accounts and a real
/// socket. So the signature is the guard: `VenueArming` rows only (label, tier, mode, block —
/// **no address**), so a book-based refusal cannot be written here without widening the signature
/// to take `vars`, a visible change at a reviewed seam.
#[must_use]
pub fn accounts_to_mount(rows: &[vike_config::VenueArming]) -> Vec<AccountLabel> {
    rows.iter()
        .filter(|row| row.is_default_account() || row.effective != vike_config::VenueMode::Paper)
        .map(|row| row.label.clone())
        .collect()
}
/// [`make_engine_with_legs`] for ONE named ACCOUNT of the venue; that function delegates here at
/// [`AccountLabel::Default`], which keeps a single-account box byte-identical. `account` selects:
///
/// * the account's TIER: [`account_tier`], the `account` rows of `(venue, label)` (the DEFAULT
///   account's are the rows with no label);
/// * the credentials the bridge reads (`load_credentials_for_account`: the default account's key
///   names are unchanged, `vike_model::accounts::account_keys::account_key` returns its input);
/// * the per-account exposure ceiling (the account rows' `max_exposure`) on the limits;
/// * the engine's `route_key` and its `live_venues` entry,
///   [`vike_model::accounts::account_keys::AccountRef::route_key`] — the bare venue id for the
///   default account, so `vike_ops::live_lock`'s `LIVE-<route_key>.lock` sentinel does not move.
///
/// ⚠ It mounts whatever it is asked to. **The shared-BOOK report is NOT made here**: it is a fact
/// about the SET of accounts, which [`make_engine_accounts`] (every composition root's entry
/// point) sees and this function does not.
pub fn make_engine_for_account(
    env: &mut MountEnv<'_>,
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
) -> Result<EngineAndRecon, MountError> {
    // ⚠ NOT named `live_events`, and the name is the mechanism: the only `live_events` binding here
    // is the account-scoped lane below ([`account_event_sender`]), so deleting it fails to compile
    // (the `ContractCall` field) instead of silently reverting to the unscoped lane — a mutation no
    // test in this crate can observe, since reaching a venue arm needs credentials and a socket.
    let venue_events = env.live_events;
    let (registry, vars, recon_enabled) = (env.registry, env.vars, env.recon_enabled);
    let (risk_profile, policy) = (env.risk_profile, env.policy);
    // The halt-admit mode in force, its DEGRADE reported once, here, at mount. Whether `verify`
    // ARMED is answered later (`report_halt_admit_armed` in `assemble_engine`): this runs before
    // credentials and cTrader's blocking handshake, either of which can land the venue on paper.
    let halt_admit = report_halt_admit(venue, policy);
    // ---- THE ACCOUNT'S TIER ----
    // ⚠ THE ONE SEAM, ABOVE the credential read on purpose: everything below (credentials, the
    // `SymbolProperties` pre-fetch, the pre-connect budget refusal, every arm's blocking
    // handshake) is work for an account the operator may not have armed, and discarding the client
    // afterwards still leaves an authenticated session on a real account (MEASURED on the CI box: a
    // one-venue run profile printed
    // `live_venues={hyperliquid,deribit,okx,bybit,alpaca,aster,binance,ig,oanda}`).
    // `env.policy == None` reads PAPER: widening has to be typed. Every production mount passes
    // `Some` (`crate::build_node` from `NodeConfig::policy`), so `None` is test callers — exactly
    // the population that must not silently arm. No row, an inactive row, a `paper` tier or two
    // active non-paper tiers all read PAPER, each with its own `block`.
    let (mode, block) = account_tier(policy, venue, account);
    // The ROUTING identity: the bare venue id for the default account, `venue#LABEL` otherwise.
    // Resolved ONCE, threaded into the engine and `live_venues`.
    let route_key = account_route_key(venue, account);
    // …and the EXEC-EVENT LANE scoped to it: every arm pushes account-tagged frames with no
    // bridge knowing accounts exist. [`account_event_sender`] says why it is unconditional and
    // inert for a default account.
    let live_events = &account_event_sender(venue_events, &route_key);
    if mode == vike_config::VenueMode::Paper {
        report_capped_to_paper(registry, venue, account, block, vars, policy);
        // ⚠ THE ROUTE KEY IS STAMPED ON THIS PATH TOO (a mutation test found it missing):
        // `ExecutionEngine::new` seeds `route_key = venue`, WRONG for a labelled account, and two
        // engines sharing one make the second unreachable
        // (`vike_core::CoreThread::engine_idx_for_route_key` returns the first match). No labelled
        // account reaches here today; this line stops that becoming a defect the day one does, and
        // makes the route-key assertion in `crates/vike-tradehub/tests/account_fanout.rs` exercise
        // `account_route_key` rather than `ExecutionEngine::new`'s seed.
        let (mut engine, recon) =
            paper_engine(venue, symbol, account, declared_legs, risk_profile, policy);
        engine.route_key = route_key;
        return Ok((engine, recon));
    }
    // The tier reaches the bridge as one bool: its LIVE tier only for a `live` account.
    let live_permitted = tier_permits_live(mode);
    // The static fee schedule, evaluated ONCE: paper arms fill with it, `resolve_fee_schedule`
    // prefers a live rate over it. KEYED BY LANE, not the bare venue: binance and aster mount SPOT
    // and USDⓈ-M PERP under one id (a trailing `.P`), and binance's maker fee differs 5x between
    // them — keyed by venue, every `BTCUSDT.P` paper/backtest mount filled at SPOT fees. `fee_lane`
    // is the identity for every other venue and every non-`.P` symbol.
    let static_default = vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    // THE VENUE'S OWN HALF: a contract row mounts through its bridge's `VenueMount`, any other row
    // is the paper client; both produce `MountParts`, so the shared tail cannot drift
    // (docs/decisions/0096).
    let parts = match row_of(registry, venue) {
        Some(VenueRow::Mount(row)) => contract::contract_parts(
            *row,
            contract::ContractCall {
                registry,
                venue,
                symbol,
                account,
                declared_legs,
                vars,
                live_events,
                recon_enabled,
                // CLONED per mount; the originals stay in `env` for the caller's next mount.
                recon_trigger: env.recon_trigger.clone(),
                properties_rec: env.properties_rec.clone(),
                risk_profile,
                policy,
                tier: mode,
                live_permitted,
                halt_admit,
                static_default,
            },
        )?,
        // A venue whose bridge this build does not compile, or one the registry does not carry (a
        // test's planted id): the paper client.
        Some(VenueRow::FeatureAbsent { .. }) | None => {
            contract::absent_parts(venue, symbol, static_default)
        }
    };
    assemble_engine(
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

#[cfg(test)]
pub(crate) use assemble::engine_mode;
