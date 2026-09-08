//! vike-mount — the venue → `ExecutionClient`/`ReconClient` composition root, extracted from
//! `vike-app`'s `main.rs` as a pure behavior-preserving move so its live-exec wiring finally sits in
//! a CI-gated library (vike-app is compile-checked only — the wgpu build weight keeps it out of the
//! test/clippy lane).
//!
//! [`make_engine`] is the one entry point: given a `venue`/`symbol` + the workspace `.env` map, it
//! builds ONE type-erased [`vike_exec::ExecutionEngine`] (Box`<dyn ExecutionClient>`) plus an
//! optional [`vike_exec::recon::ReconClient`] for that venue. The LIVE GATE is TWO gates now, and
//! the order between them is the point: the deployment's per-venue ARMING CEILING
//! (`policy.venues.<venue>`, `paper` < `demo` < `live`) is consulted FIRST, above the credential
//! read — a `paper` venue returns the paper client having loaded no credential, fetched no
//! instrument grid and opened no socket. Only then does the older rule apply,
//! absent-credentials-is-the-live-gate: a venue whose `{VENUE}_DEMO_*` keys (or Aster's
//! agent-wallet keys / Hyperliquid's private-key shape) are present spawns a real credential-gated
//! exec client; otherwise the venue stays PAPER (`vike_paper::PaperExecutionClient`).
//!
//! ⚠ The ceiling can only ever REFUSE (`vike_config::VenueMode::cap` is `min`, never `max`), so
//! `venues.bybit = "live"` does not put bybit live — it declines to stop bybit going live. It
//! binds at four places beyond the top-of-function refusal, and each is a venue whose arm picks its
//! own tier: the three CEX venues' `{VENUE}_MAINNET` conjunct, hyperliquid's same flag inside
//! [`hyperliquid::hl_env`], aster's Live-first chain (which has NO flag, and is the measured hole
//! this stage closes), and polymarket's exec, whose only tier is real money on Polygon.
//!
//! Layout (ReconFactory seam, wave-2 task 6): every venue's `ReconClient` is now built by that
//! venue's OWN `pub fn recon_client(...)` factory, living in its bridge crate next to the
//! `ReconClient` impl it constructs — this crate no longer holds any signer/transport/URL wiring
//! detail. [`recon::build_recon_client`] is a thin bybit/okx/binance dispatch (kept for its existing
//! callers/signature); the Hyperliquid bespoke-signer live+recon builder is
//! [`hyperliquid::hyperliquid_live_client`] (calls `vike_hyperliquid::recon_client` once its
//! `instruments` fetch resolves `product` — see that fn's doc for why the factory can't resolve
//! `product` itself without a second network round-trip); the per-venue fallback `SymbolProperties`
//! grids are in [`fallback`]. Deribit, Aster, cTrader, Alpaca, IG, OANDA (and, behind the `ibkr` /
//! `polymarket` features, IBKR and Polymarket) call their OWN `recon_client` factory INLINE in
//! `make_engine`'s arms — each needs a
//! non-REST, bespoke-signer, or non-generic-credentials handshake `build_recon_client` can't produce
//! (cTrader's is a protobuf/TLS OAuth handshake over its own dedicated authed socket; IG/OANDA use
//! their own `{IG,OANDA}Config` shapes over a dedicated logged-in `IgSession` / Bearer `OandaRest`,
//! never the exec side's transport; Polymarket needs an L1→EOA derivation plus a blocking
//! `/auth/derive-api-key` L2 round-trip off the workspace `.env` map).
//!
//! Five of those inline factories — deribit / cTrader / IG / OANDA / IBKR — perform a BLOCKING
//! AUTHENTICATED handshake to construct the client, so they are gated on [`make_engine`]'s
//! `recon_enabled` (the caller's already-resolved `vike_ops::reconcile_config::reconcile_gate`
//! verdict — ON by default for a live mount since S2) through
//! `recon_if_enabled`: reconcile off ⇒ the factory is never called and the venue does no
//! authenticated network work at mount. That gate is an explicit PARAMETER, deliberately NOT
//! `recon_trigger.is_some()` — the trigger is a per-venue RECONNECT poke wired for only four venues,
//! and `vike_run::build_node` passes `None` for these five even with reconciliation fully on, so
//! inferring the global gate from it would silently stop reconciling them.
//!
//! Polymarket is the ONE venue with **no testnet**, so its arm is gated TWICE where every other
//! venue is gated once: `POLY_EXEC=1` mounts the real (REAL-MONEY, Polygon-mainnet)
//! `ExecutionClient` — whose exec thread also owns the authenticated user-WS fill pump, the only
//! lane a Polymarket fill/cancel ever arrives on — and `POLY_RECONCILE=1`, on top of the master
//! gate, mounts the `ReconClient` (`poly_recon_wanted`; ⚠ that arm read the venue flag ALONE
//! until 2026-09-06, which built a handle a driver-less root then dropped, and since S2 the master
//! gate is ON BY DEFAULT for a live mount — so `POLY_RECONCILE=1` is now the ONE act this venue
//! still needs where it used to be the second of two). With both on, ONE
//! `vike_polymarket::live_mount_from_vars` call builds them over
//! the same L2 handshake and the same `PolymarketRegistry` (so order reports re-key to local coids).
//! With reconcile on but exec OFF it is the pre-existing recon-only shape — live venue state diffed
//! against a PAPER engine, which must run under `VIKE_RECONCILE_POLICY=quarantine`
//! (`vike_polymarket::poly_reconcile_enabled`'s doc is the authority on why). Both unset (the
//! default) ⇒ no client, no network call, venue stays paper.
//!
//! Env boundary: the opt-in PIT-`SymbolProperties` recorder is CONSTRUCTED by the calling binary
//! (`vike_data::PropertiesRecorder::open_from_env` — gated on `VIKE_RECORD_PROPERTIES=1` AND on
//! vike-data's `hist-datafusion` feature, which only the binaries carry) and passed in via
//! `properties_rec`; this crate only THREADS the ungated recorder handle into the venue spawns, so
//! its vike-data dep stays trait-only (no DataFusion through this edge). The tick-recording store
//! root is likewise passed in by the caller (`tick_store_root`) — `vike-app` still owns the READS
//! that resolution needs (the variable, the boot's settings directory, its own executable path)
//! because it uses the same root elsewhere; the LADDER itself is
//! `vike_model::tick_store_path::resolve_tick_store_root`, shared with the daemon's twin.
//! Every `{VENUE}_MAINNET` flag keeps its reads at its own site (`std::env::var` plus the `.env`-map
//! lookup — `hyperliquid::hl_env` here, each CEX bridge's own `mainnet_enabled` there); only the
//! PARSE is shared, through `vike_bridge_core::mainnet`'s ONE converged rule (STEP 2: the exact
//! string `"1"`, process env OR the workspace `.env` map, process winning — no venue diverges any
//! more). For the three CEX venues [`make_engine`] resolves that flag EXACTLY ONCE per mount
//! (`cex_mainnet_enabled`) and threads the resulting `bool` into the grid pre-fetch, the exec spawn
//! and [`recon::build_recon_client`], so no spawned adapter thread re-reads global env and a mount
//! can never sign mainnet credentials against demo hosts.
//!
//! Settings (settings-unification Phase 6c): [`make_engine`] takes a [`MountPolicy`] — the
//! projection of `vike_config::Policy` this mount APPLIES. Same env boundary as everything above:
//! the BINARY loads `<vike home>/policy.toml` (`vike_config::load` takes the environment as a MAP)
//! and passes the projection in; this crate never reads `std::env` for it. Phases 1–5 built the
//! whole typed settings system and no venue mount ever read a `Policy` — this is the edge that
//! changes that, and [`policy`]'s module doc is the authority on which fields are carried, which
//! are deliberately not, and why. `None` (no policy file) is byte-identical to every mount before
//! the parameter existed.
//!
//! Startup safety (two seams that used to exist with no caller, now wired here — the mount IS the
//! composition root, so this is where a "before the first live order" check belongs):
//! [`preflight`] is the pure go/no-go gate (moved down from `vike-app-core`) and [`startup`] is its
//! real-probe wiring, run once by `vike_run::build_node`; and this file's binance live arm now
//! consults [`vike_bridge_core::key_permissions`] before arming, so a KNOWN withdraw-capable API
//! key degrades that venue to paper instead of being discovered at the first order — see
//! `arming::binance_withdraw_gate`.

use std::collections::{HashMap, HashSet};

use vike_bridge_core::key_permissions::WithdrawGate;
use vike_model::account_keys::AccountLabel;

// `all(test, …)`, not `feature` alone: `fxcm_live_intent`'s CONSUMER moved to `arming.rs` with the
// function, so the only uses left in THIS file are its two `#[cfg(feature = "fxcm")]` tests
// (`fxcm_credentials_without_a_linked_sdk_refuse_the_live_mount` and
// `fxcm_probes_live_only_with_credentials_and_a_linked_sdk`), which reach it via `super::`. Gated on
// the feature alone, an `--features fxcm` LIB build imports a name it never uses and `-D warnings`
// refuses it — which is exactly how the fxcm lane found this.
#[cfg(all(test, feature = "fxcm"))]
use arming::fxcm_live_intent;
use arming::{
    account_arming_under, account_ceiling, account_event_sender, account_route_key,
    arm_universal_defaults, binance_withdraw_gate, ceiling_permits_live, cex_cred_choice,
    cex_mainnet_enabled, margin_mode_grid, multiplier_grid, require_live_risk_budget,
    venue_ceiling, would_mount_live_under_policy,
};
use paper_fallback::{
    paper_client, paper_engine, report_capped_to_paper, report_halt_admit, report_halt_admit_armed,
    venue_arming_migration,
};

mod arming;
pub mod book_identity;
mod error;
mod fallback;
pub mod hyperliquid;
mod paper_fallback;
pub mod policy;
pub mod preflight;
mod recon;
pub mod server_time;
pub mod startup;
pub mod symbol_grid;

pub use arming::{
    known_accounts, resolve_fee_schedule, shared_book_ceiling_note, shared_books_for,
    symbol_for_account, venue_account_arming, venue_arming, would_mount_live,
    would_mount_live_under,
};
pub use policy::MountPolicy;
pub use recon::build_recon_client;
pub use symbol_grid::{DeclaredGridSource, declared_grid_source};
/// The arming VOCABULARY, re-exported because it appears in THIS crate's public signatures.
///
/// [`venue_arming`] returns `Vec<vike_config::VenueArming>`, so a consumer can already hold these
/// values — it just could not NAME them without taking a `vike-config` dependency of its own.
/// `vike-run` needed exactly that to write the `venue_mounted` journal records, and a manifest edge
/// added for a type name is worse than the re-export: it widens the dependency graph to say
/// something the signature already says.
///
/// ⚠ Not a `pub use` SHIM in the sense `CLAUDE.md` forbids — nothing MOVED here and no old spelling
/// is being kept alive. This is the module-vocabulary exception that rule names, the same shape as
/// `vike_exec::ExecutionClient`.
pub use vike_config::{ArmingBlock, VenueArming, VenueMode, VenuePolicy};

/// The operator HALT sentinel, re-exported for the mount seams that live ABOVE this crate.
///
/// `vike-run` builds its own paper mount (`build_paper_maker_core_with` — the one `vike-tradehub`'s
/// paper daemon runs) without going through [`make_engine`] at all, so it has to arm the SAME
/// sentinel; it depends on vike-mount but deliberately not on vike-bridge-core, whose
/// ureq/tungstenite/rustls stack it has no other use for. Re-exporting here is what lets both mount
/// seams resolve ONE path (`halt::halt_path_from_env`) without a second crate learning about the
/// transport tree, and without a second copy of the three-rung precedence existing anywhere.
pub use vike_bridge_core::halt;

/// The ONE gate the five INLINE-recon venue arms (deribit / ctrader / ig / oanda / ibkr) apply
/// before their blocking, authenticated `ReconClient` handshake: build it only when reconciliation
/// is actually on, otherwise leave the venue reconcile-inert exactly as a paper venue is.
///
/// Generic over the arm's own factory CLOSURE rather than taking an already-built
/// `Option<Box<dyn ReconClient>>`, and that is the entire point: the property this exists to hold is
/// that the factory is never *called* when `recon_enabled` is false. It cannot be observed from the
/// returned `Option` alone — an unreachable venue, absent credentials and a disabled gate all return
/// `None` — so a laziness bug (constructing the client and then discarding it, which is precisely
/// what these five arms used to do) would be invisible to any assertion on the result. A closure
/// makes it directly checkable offline, with no network and no credentials: call this with a
/// counting closure and assert the count (`recon_if_enabled_is_lazy_when_disabled` below).
///
/// `true` is a pure pass-through — `build()` is invoked with the caller's own arguments in the
/// caller's own position, so the enabled path is byte-identical to calling the factory inline.
fn recon_if_enabled<F>(
    recon_enabled: bool,
    build: F,
) -> Option<Box<dyn vike_exec::recon::ReconClient>>
where
    F: FnOnce() -> Option<Box<dyn vike_exec::recon::ReconClient>>,
{
    if recon_enabled { build() } else { None }
}

/// **Polymarket's reconcile decision, extracted so it can be tested** — `recon_enabled` (the
/// caller's already-resolved `vike_ops::reconcile_config::reconcile_gate` verdict) AND the venue's
/// own `POLY_RECONCILE=1`.
///
/// ⚠ **It reads the master gate, and until 2026-09-06 it did not.** The `("polymarket", _)` arm
/// keyed on `poly_reconcile_enabled` ALONE, and this was defended everywhere in the tree as "the
/// venue has its own equivalent inner gate". It was never equivalent: the client this arm builds is
/// only ever USED by the driver both roots mount under the master gate, so the effective condition
/// was already `POLY_RECONCILE=1 AND recon_enabled` — the arm simply performed the venue's
/// authenticated L1→EOA + `/auth/derive-api-key` round trip first and let the handle be dropped
/// when the gate said no. That is the exact build-then-discard defect
/// [`recon_if_enabled`] exists to have removed for the other five inline venues, wearing a
/// different name.
///
/// ⚠ **And S2 is what made the difference visible, in the direction an operator feels.** The master
/// gate is now ON BY DEFAULT for a mount that arms a live venue account, so a box carrying
/// `POLY_RECONCILE=1` and no `VIKE_RECONCILE` reconciles Polymarket where it used to build a client
/// and reconcile nothing — authenticated Polygon-MAINNET reads against a real-money venue with no
/// testnet. That is a real change and it is NAMED rather than smoothed over:
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the verdict and the
/// alternative the owner may prefer, and `docs/ops/reconcile-on-restart.md` tells the operator
/// which single line turns it back off (`POLY_RECONCILE` unset, or `VIKE_RECONCILE_OFF=1`).
/// What has NOT changed is that Polymarket still needs an act nobody else needs: every other venue
/// reconciles on the default alone, this one needs `POLY_RECONCILE=1` on top of it.
///
/// Feature-gated with the arm it serves: a default build compiles no `("polymarket", _)` arm, so an
/// ungated helper would be `dead_code` under the workspace's `-D warnings` clippy gate.
#[cfg(feature = "polymarket")]
#[must_use]
fn poly_recon_wanted(recon_enabled: bool, poly_reconcile: bool) -> bool {
    recon_enabled && poly_reconcile
}

/// `make_engine`'s return shape — factored into a named alias per clippy's `type_complexity` (the
/// raw nested-generic tuple reads worse spelled out at both the fn signature and every call site).
pub type EngineAndRecon = (
    vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>,
    Option<Box<dyn vike_exec::recon::ReconClient>>,
);

/// [`make_engine`]'s one failure mode (armed-risk-defaults, Task 6 of the RunProfile-wiring plan):
/// a LIVE mount refuses to start because the operator supplied no account-dependent risk budget.
/// See [`require_live_risk_budget`]'s doc for the rationale — no universal safe default exists for
/// `max_notional_per_order`/`max_total_exposure` (a Freqtrade-shaped requirement, unlike the three
/// Nautilus-shaped defaults [`arm_universal_defaults`] arms unconditionally). A paper/backtest
/// mount never sees this variant — it is raised only for a venue the operator INTENDS live:
/// pre-connect when [`would_mount_live`] says this venue's live config is present (the primary
/// site — no venue session exists yet), or at the post-merge backstop for a live arm the probe
/// does not know.
#[derive(Debug)]
pub enum MountError {
    /// `venue` names the mount that refused to start; `missing` lists EVERY missing `risk.*` key
    /// (not just the first) so one fix cycle closes the gate. `profile_supplied` records whether
    /// a `[risk]` table reached this mount AT ALL (`make_engine`'s `risk_profile` argument was
    /// `Some`) — the two cases need DIFFERENT operator instructions, and only the caller knows
    /// which one this is: no profile ⇒ "create one and point at it" (the whole file is the fix),
    /// a profile that simply omits the caps ⇒ "add these lines to the file you already have".
    MissingRiskBudget { venue: String, missing: Vec<&'static str>, profile_supplied: bool },
}

/// The commented, copy-pasteable live profile shipped in-tree, named by the diagnostic below.
/// `crates/vike-run/tests/risk_budget_diagnostic.rs` reads this path back OUT of the rendered
/// message and parses the real file as a `vike_core::RunProfile` (vike-run is the lowest crate
/// that depends on BOTH halves), so this reference cannot rot into a dangling one.
const EXAMPLE_PROFILE_PATH: &str = "docs/ops/run-profile-live.toml";

/// Example value + one-line meaning per account-dependent cap, so the diagnostic's inline `[risk]`
/// table is a working starting point rather than a bare key list. Keyed by the SAME `&'static str`
/// names [`require_live_risk_budget`] pushes into `missing` — `every_missing_key_has_an_example`
/// pins that correspondence, so a third cap added there fails this table's test until it gains a
/// row (the row is what makes the message copy-pasteable, not decoration).
/// ⚠ `max_total_exposure`'s meaning is written to match what the gate ACTUALLY evaluates. Its
/// name reads account-wide and this line used to say "cap on total open notional across venues",
/// which no code has ever enforced: `vike_exec::RiskGate::check_inner`'s `over-max-exposure` lane
/// prices `(ctx.position_size + side*qty).abs() * ctx.mark_price * ctx.multiplier`, and
/// `RiskContext::position_size` is "current SIGNED position in the symbol" — ONE symbol, in ONE
/// engine, which `make_engine` builds per venue. An operator who read "across venues" would size
/// this for their whole book and get the cap applied per symbol instead, i.e. N times looser than
/// the number they wrote.
/// ⚠ The cross-symbol lane this comment used to call "a separate epic" — on the grounds that it
/// needed the position book threaded into `check()`, inside the `p99 < 10µs` fold — EXISTS, and
/// that objection turned out not to apply: `vike_exec::RiskLimits::max_account_exposure` reaches
/// the gate as one pre-folded scalar on the `Copy` `vike_exec::RiskContext`, computed on the cold
/// per-order path beside equity and margin-in-use, so nothing joined the measured hop. It is a
/// `policy.toml` key rather than a `[risk]` one and is deliberately NOT part of the refusal below,
/// so this table stays the two caps a live mount demands.
const BUDGET_EXAMPLES: &[(&str, &str, &str)] = &[
    ("max_notional_per_order", "5000.0", "cap on ONE order's notional"),
    ("max_total_exposure", "25000.0", "cap on ONE symbol's projected open notional"),
];

#[allow(clippy::too_many_arguments)]
pub fn make_engine(
    venue: &str,
    symbol: &str,
    vars: &HashMap<String, String>,
    live_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_enabled: bool,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<EngineAndRecon, MountError> {
    make_engine_with_legs(
        venue,
        symbol,
        &[],
        vars,
        live_events,
        live_venues,
        recon_enabled,
        recon_trigger,
        properties_rec,
        risk_profile,
        policy,
    )
}

/// [`make_engine`] for a mount that trades MORE THAN ONE symbol on this venue: `declared_legs` are
/// the EXTRA symbols (beyond `symbol`) the caller's `StrategyMount`s declared for it, and the only
/// thing they change is `vike_exec::RiskLimits::grid_by_symbol` — the per-symbol PRICE/SIZE GRID
/// the `RiskGate` rounds each order onto.
///
/// **Why this exists as its own entry point rather than an 11th parameter on [`make_engine`].**
/// The single-symbol mount is the overwhelming majority (every call site in this workspace but
/// `vike_run::build_node`'s), and it must be BYTE-IDENTICAL — so it keeps its signature and reaches
/// this function with an EMPTY slice, which provably touches nothing (`declared_symbol_grids`
/// returns an empty map without calling the venue at all, and an empty `grid_by_symbol` carries
/// `skip_serializing_if`, so even `vike_exec::engine_snapshot::state_hash` is unchanged). Same
/// shape, and the same reason, as `vike_bybit::exec::fetch_bybit_properties_with_cap` beside its
/// plainer twin: widen the caller that needs the extra fact, leave the ~19 that do not untouched.
///
/// ⚠ **Not every arm can honour a declared leg, and the ones that cannot SAY so.** A leg is gridded
/// only from a source the arm ALREADY holds — no mount gains a blocking network round trip per
/// declared leg. [`symbol_grid`]'s module doc is the authority, [`declared_grid_source`] is the
/// per-venue declaration, and `symbol_grid::warn_ungridded_legs` is the one line an operator reads
/// when a leg falls back to the mounted symbol's grid (which is what EVERY leg did before this
/// function existed — an ungridded leg is degraded, never newly broken).
///
/// A leg naming `symbol` itself is ignored (the scalars already are that symbol's grid), as is a
/// blank one and a repeat, so a caller may pass its mount's raw leg list through unfiltered.
///
/// ⚠ **It mounts the venue's DEFAULT account and only that one.** The per-account fan-out is
/// [`make_engine_accounts`]; this signature is kept, unchanged, because ~19 call sites in this
/// workspace mount the one account they have ever had, and every one of them must stay
/// byte-identical. `AccountLabel::Default` is not "no account" — it is THE account a single-account
/// box has, and [`make_engine_for_account`] renders it exactly as this function always rendered it.
#[allow(clippy::too_many_arguments)]
pub fn make_engine_with_legs(
    venue: &str,
    symbol: &str,
    declared_legs: &[String],
    vars: &HashMap<String, String>,
    live_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_enabled: bool,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<EngineAndRecon, MountError> {
    make_engine_for_account(
        venue,
        symbol,
        &AccountLabel::Default,
        declared_legs,
        vars,
        live_events,
        live_venues,
        recon_enabled,
        recon_trigger,
        properties_rec,
        risk_profile,
        policy,
    )
}

/// **THE FAN-OUT: one engine per ACTIVE account of `venue`.**
///
/// The venue's DEFAULT account is always mounted, at whatever tier it resolves to — including
/// `paper`, because that is the engine every caller has always received and the one a paper box
/// trades on. Every LABELLED account is mounted only when it is ACTIVE: its per-account ceiling
/// permits something above paper AND its credentials load. A labelled account that fails either
/// produces NO engine at all — a paper engine for an account nobody armed is a second local book
/// nobody asked for, and `vike_core`'s mount resolution would then bind a strategy to it.
///
/// **Result order is the contract**: the default account is FIRST, always, and the labelled ones
/// follow in label order. `vike_run::build_node` binds `[0]` to the engine it has always bound and
/// pushes the rest onto its `extra` list, so a box with one account per venue produces the identical
/// engine vector it produced before this function existed.
///
/// # ⚠ NOTHING HERE IS REFUSED FOR SHARING AN INSTRUMENT — that rule is GONE
///
/// This fan-out used to cap a labelled account to `paper` when another active account of the venue
/// was armed on its symbol. **Two accounts on one instrument is an ordinary spread** (long BTC on
/// A, short BTC on B): two accounts are two wallets, they hold separate positions, and there was
/// nothing to refuse. `vike_config::venue_accounts`' module doc carries the correction in full.
///
/// What survives is the hazard that is real — two accounts resolving to ONE venue BOOK — and it is
/// **reported, never refused**: one `warn!` per pair naming the venue, both labels and the shared
/// book, and both engines mounted (`docs/decisions/0013-degrade-vs-refuse.md`;
/// [`venue_arming_migration`] is the precedent). Where `book_identity` cannot determine the book
/// offline, nothing is said and both mount — an unprovable suspicion is not a finding.
///
/// # ⚠ Each account is mounted on ITS OWN symbol
///
/// `account_symbols` is one row per account — the DEFAULT account on the venue's wired symbol, a
/// labelled account on the symbol the strategy mount that NAMED it trades
/// (`vike_run::account_symbols_for` is the derivation). It replaced a single `symbol: &str`
/// parameter handed to every account of the venue, which is what made a labelled account
/// unaddressable OUTBOUND: it was mounted on a symbol its own strategy had not chosen. A one-entry
/// `[(AccountLabel::Default, symbol)]` is byte-identical to that parameter, which is what every
/// caller with no labelled mount passes. Two accounts sharing one symbol here is legal and
/// expected.
#[allow(clippy::too_many_arguments)]
pub fn make_engine_accounts(
    venue: &str,
    account_symbols: &[(AccountLabel, String)],
    declared_legs: &[String],
    vars: &HashMap<String, String>,
    live_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_enabled: bool,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<Vec<(AccountLabel, EngineAndRecon)>, MountError> {
    let venue_policy = policy.map(|p| &p.venues);
    let rows = venue_account_arming(venue, vars, venue_policy);
    // THE WARNING, said in full BEFORE anything is mounted — and then BOTH accounts are mounted.
    // `SharedBook::why` names both accounts and the shared book, which is the whole difference
    // between a line an operator can act on in seconds ("that is my master address, one of these
    // two labels is wrong") and "bybit has a problem". One line per PAIR, because the pair is the
    // finding.
    //
    // ⚠ `warn!`, and the mount CONTINUES. The operator wrote both credential sets by hand under
    // explicit labels; refusing would strand a venue on paper over a configuration they may well
    // have meant (`docs/decisions/0013-degrade-vs-refuse.md`, and `venue_arming_migration` right
    // below is the same shape — name what you found, start anyway).
    //
    // ⚠ DECLARED BLIND SPOT, measured rather than assumed: a mutation that deletes this loop is NOT
    // caught by any test in this crate. What IS gated is the CONTENT — `book_identity`'s own table
    // test drives the resolution and `crates/vike-mount/tests/shared_book_report.rs` drives
    // `shared_books_for` and asserts both labels and the address appear — and the EFFECT, which is
    // [`accounts_to_mount`]: `a_shared_book_removes_no_account_from_the_mount_set` pins that this
    // finding takes no account out of the mount set, which is the mutation that matters (a deleted
    // `warn!` costs the operator a signal; a shared book that REFUSED would cost them the account).
    // Only the EMISSION is unreachable, and structurally: two accounts can only both be ACTIVE on a
    // venue with a real live arm, so a test that got this far would dial the venue on the very next
    // statement.
    // ⚠ …AND THE ACCOUNT-AGGREGATE CEILING MULTIPLIES ON EXACTLY THIS SHAPE, which is why the
    // warning carries it. `vike_exec::RiskLimits::max_account_exposure` is armed once per ENGINE
    // (`make_engine_for_account`), so two engines over ONE venue ledger apply the whole ceiling
    // twice and the real book may hold a multiple of the number the operator wrote — the
    // N×-looser defect that axis exists to close, wearing the account label. Told HERE because
    // this is the only moment the process knows both facts at once, and told at startup rather
    // than after a fill. `shared_book_ceiling_note` is empty when the ceiling is unarmed, so a
    // deployment without one reads exactly the line it always read.
    let ceiling_note = shared_book_ceiling_note(policy.and_then(|p| p.max_account_exposure));
    for shared in shared_books_for(venue, &rows, vars) {
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
    if rows.is_empty() {
        // A venue `vike_model::VENUES` does not carry — a test id, a sim id. It has exactly one
        // account, nothing can name a second, and there is no policy row to consult, so the fan-out
        // is the single mount it has always been.
        let engine = make_engine_for_account(
            venue,
            symbol_for_account(account_symbols, &AccountLabel::Default),
            &AccountLabel::Default,
            declared_legs,
            vars,
            live_events,
            live_venues,
            recon_enabled,
            recon_trigger,
            properties_rec,
            risk_profile,
            policy,
        )?;
        return Ok(vec![(AccountLabel::Default, engine)]);
    }
    // WHICH accounts get an engine — decided ONCE, by a function that cannot see a book. Every
    // label below is mounted; there is no second filter, and adding one here would be adding a
    // filter to a loop over an already-decided set.
    let mut out = Vec::with_capacity(1);
    for label in accounts_to_mount(&rows) {
        // …each on ITS OWN symbol. The DEFAULT account's row is the venue's wired market; a
        // labelled account's is the symbol the mount that named it trades, so the two engines mount
        // two instruments and `ExecutionEngine::accepts_symbol` answers for each of them separately.
        let engine = make_engine_for_account(
            venue,
            symbol_for_account(account_symbols, &label),
            &label,
            declared_legs,
            vars,
            live_events,
            live_venues,
            recon_enabled,
            recon_trigger.clone(),
            properties_rec.clone(),
            risk_profile,
            policy,
        )?;
        out.push((label, engine));
    }
    Ok(out)
}

/// **WHICH accounts of a venue get an ENGINE** — [`make_engine_accounts`]' whole mount decision,
/// lifted out of its loop so it can be tested at all.
///
/// The rule is two lines and has not changed: the DEFAULT account is mounted unconditionally — it
/// is the engine this fan-out's single-account predecessor always returned, and every caller binds
/// it at `[0]` — and a LABELLED account is mounted exactly when it ARMED
/// (`vike_config::VenueArming::effective` above `Paper`), because a labelled account that resolved
/// paper has no credentials, no ceiling, or no line naming it.
///
/// # ⚠ Why this is a named function and not four lines inside the loop
///
/// **A shared BOOK must never remove an account from this set** — two accounts of one venue
/// resolving to one effective trading address is a WARNING and a mount, not a refusal
/// (`docs/decisions/0013-degrade-vs-refuse.md`; [`shared_books_for`] computes the report and
/// [`make_engine_accounts`] emits it). That property was *stated* in a comment and gated by
/// nothing: dropping every `SharedBook::second` from the mount loop — the operator's second account
/// silently never mounted, which is the paste-error signal turned into a paste-error *outcome* —
/// was measured GREEN across this crate and `vike-run`, because reaching that loop needs two ACTIVE
/// accounts and therefore a real socket.
///
/// So the decision moved somewhere a test can reach, and the signature is the real guard: this
/// function takes `vike_config::VenueArming` rows and NOTHING else. A row carries a label, a
/// ceiling, an effective mode and a block — **no address**. The shared-book fact is not merely
/// unused here, it is unavailable, so a future refusal cannot be written into this function without
/// first widening its signature to take the `vars` the book is derived from, which is a visible
/// change at a reviewed seam rather than a `continue` inside a loop.
///
/// Order is the row order, so the default account stays first — load-bearing, see
/// [`make_engine_accounts`].
#[must_use]
pub fn accounts_to_mount(rows: &[vike_config::VenueArming]) -> Vec<AccountLabel> {
    rows.iter()
        .filter(|row| row.is_default_account() || row.effective != vike_config::VenueMode::Paper)
        .map(|row| row.label.clone())
        .collect()
}
/// [`make_engine_with_legs`] for ONE named ACCOUNT of the venue.
///
/// **This is the body [`make_engine_with_legs`] used to be**; that function is now a one-line
/// delegation at [`AccountLabel::Default`], and the delegation is what keeps a single-account box
/// byte-identical. Three things read `account`, and nothing else in the ~1200 lines below does:
///
/// * the ARMING CEILING seam consults [`account_ceiling`] instead of [`venue_ceiling`] — the same
///   fold one level down, `min`-capped by the venue's own line exactly as before;
/// * the credential read is `load_credentials_for_account`, which for the default account builds
///   the same key names it always did (`vike_model::account_keys::account_key` returns its input
///   unchanged for `Default`);
/// * the engine's `route_key` and its entry in `live_venues` become
///   [`AccountRef::route_key`] — the bare venue id for the default account, so
///   `vike_ops::live_lock`'s `LIVE-<route_key>.lock` sentinel does not move for any existing
///   deployment.
///
/// ⚠ It mounts whatever it is asked to mount. **The shared-BOOK report is NOT made here** — it is a
/// fact about the SET of accounts on a venue, which this function cannot see one account at a time.
/// [`make_engine_accounts`] is the fan-out that owns it, and it is the entry point every
/// composition root uses.
#[allow(clippy::too_many_arguments)]
pub fn make_engine_for_account(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    vars: &HashMap<String, String>,
    // ⚠ NOT named `live_events`, and the name is the mechanism. Every venue arm below reaches its
    // exec-event lane by the name `live_events`, which in this function is bound ONLY by the
    // account-scoped shadow further down ([`account_event_sender`]). Naming the PARAMETER something
    // else is what makes deleting that shadow a compile error at fourteen call sites instead of a
    // silent revert to the unscoped lane — a mutation no test in this crate can observe, because
    // reaching a venue arm at all needs real credentials and a real socket.
    venue_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_enabled: bool,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<EngineAndRecon, MountError> {
    use vike_bridge_core::credentials::{Environment, load_credentials_for_account};
    // Go-live credential switch (completes #771). #771 flipped binance/bybit/okx ENDPOINTS onto
    // mainnet under `{VENUE}_MAINNET=1`; this is the matching CREDENTIALS switch, reading the SAME
    // flag from the SAME reader (`cex_mainnet_enabled` delegates to each bridge's own
    // `mainnet_enabled`), so the credentials a mount signs with and the hosts the adapter binds
    // always flip together. Flag SET ⇒ load the venue's LIVE (mainnet) key set; flag UNSET — the
    // default, and EVERY non-CEX venue — ⇒ load DEMO exactly as before, so the unset path is
    // byte-identical. SAFETY (absent-credentials-is-the-live-gate): a set flag with NO live creds
    // resolves to `None` here and stays PAPER — it never signs a mainnet host with demo keys. See
    // `cex_cred_choice` for the pure flag×creds matrix + its test.
    //
    // STEP 2 of the `{VENUE}_MAINNET` convergence: this ONE resolution (now reading the process env
    // AND `vars`, the workspace `.env` map) is the venue's single source of truth for the whole
    // mount — it is threaded below into the grid pre-fetch, the exec spawn and `build_recon_client`,
    // so no spawned thread re-reads the flag for itself.
    // The halt-admit mode in force at this venue, with the DEGRADE reported once, here, at mount —
    // never at the first halted submit. Whether `verify` actually ARMED is a different question and
    // is deliberately NOT answered here: this line runs before credentials and before cTrader's
    // blocking handshake, either of which can land the venue on paper. `report_halt_admit_armed`
    // says that at the cTrader arm, and both docs carry the reasoning.
    let halt_admit = report_halt_admit(venue, policy);
    // ─── THE ARMING CEILING (settings-unification stage 3) ────────────────────────────────────
    //
    // ⚠ THE ONE SEAM, and it is ABOVE the credential read on purpose. Everything below this block
    // — `load_credentials_from`, the half-credential report, the `SymbolProperties` pre-fetch, the
    // pre-connect budget refusal, every venue arm's blocking handshake — is work done ON BEHALF of
    // a venue the operator may have disarmed. A ceiling that let all of that run and then discarded
    // the client would still be an authenticated session on a real account, which is the entire
    // defect (MEASURED on the CI box: a one-venue run profile printed
    // `live_venues={hyperliquid,deribit,okx,bybit,alpaca,aster,binance,ig,oanda}`).
    //
    // `None` — a caller that threads no policy — reads PAPER, not `Live`. Fail-safe by
    // construction: the widening mistake has to be typed, it cannot be reached by omission. Every
    // production mount does pass `Some` (`vike_run::build_node` from `NodeConfig::policy`, and
    // `vike-app`/`vike-tradehub` build that from their own `vike_config::load`), so the `None` arm
    // is test callers and future ones — exactly the population that must not silently arm.
    // …and it is now per ACCOUNT. `account_ceiling` is `min(the venue's line, this account's own
    // line)` — a second `min` under the first, so the seam can still only ever REFUSE. For
    // `AccountLabel::Default` it IS `venue_ceiling`, by construction rather than by care
    // (`VenuePolicy::account` returns the venue ceiling exactly when the label is the default one),
    // which is what leaves a box with no `[accounts]` table on the identical path.
    let mode = account_ceiling(policy, venue, account);
    // The ROUTING identity of this mount — the bare venue id for the default account, `venue#LABEL`
    // for a labelled one. Resolved ONCE here and threaded into `live_venues` and the engine below,
    // rather than re-rendered at each of the fourteen sites that record a live arm.
    let route_key = account_route_key(venue, account);
    // …and the EXEC-EVENT LANE is scoped to it, once, here. **This is the only binding of the name
    // `live_events` in this function** (the parameter is `venue_events` — see its own note), so
    // every venue arm below pushes account-tagged frames without any arm, and without any bridge,
    // knowing that accounts exist; and deleting this line does not silently revert to the unscoped
    // lane, it fails to compile. See [`account_event_sender`] for why it is unconditional and why
    // a default account's lane is byte-identically inert.
    let live_events = &account_event_sender(venue_events, &route_key);
    if mode == vike_config::VenueMode::Paper {
        // THE UPGRADE WARNING, once per process and before the per-venue line: with no `[venues]`
        // table written, EVERY venue reads `paper`, so the first mount is always in this branch and
        // is always the right place to say it. See `venue_arming_migration`'s doc for why it warns
        // rather than refusing, and for what silences it.
        venue_arming_migration(vars, policy);
        report_capped_to_paper(venue, vars, policy);
        // ⚠ THE ROUTE KEY IS STAMPED ON THIS PATH TOO, and it was not until a mutation test found
        // it. `paper_engine` builds through `ExecutionEngine::new`, which seeds `route_key` equal
        // to `venue` — correct for the default account and WRONG for any other, because two engines
        // sharing a route key make the second unreachable for every venue-tagged payload
        // (`vike_core::CoreThread::engine_idx_for_route_key` returns the first match). Nothing
        // reaches here with a labelled account today — `make_engine_accounts` mounts no paper
        // second account — so this is not a live defect; it is the ONE line that stops it from
        // becoming one the moment something does, and it is what makes the roster-wide route-key
        // assertion in `crates/vike-mount/tests/account_fanout.rs` actually exercise
        // `account_route_key` rather than `ExecutionEngine::new`'s seed.
        let (mut engine, recon) = paper_engine(venue, symbol, declared_legs, risk_profile, policy);
        engine.route_key = route_key;
        return Ok((engine, recon));
    }
    // THE FOLD, spelled as the one method that spells it. `VenueMode::cap` is `min`, never `max`
    // (its doc explains why it exists as a named method rather than as a `.min()` at each site):
    // the highest tier the venue's own mechanisms could reach is LIVE, and the ceiling caps it. A
    // reviewer looking for a widening bug is looking for a `max`, and there is one place to look.
    let live_permitted = ceiling_permits_live(mode);
    // The `{VENUE}_MAINNET` switch is now a CONJUNCT, not the whole answer: under a `demo` ceiling
    // an armed flag selects nothing. `BINANCE_MAINNET=1` + `venues.binance = "demo"` therefore
    // mounts the DEMO tier — and if no demo keys exist, `load_credentials_from` answers `None` and
    // the venue stays PAPER, because a mainnet host is never signed with demo keys
    // (`cex_cred_choice`'s `MainnetNoCreds`, which the `warn!` below still reports).
    let mainnet = cex_mainnet_enabled(venue, vars) && live_permitted;
    let tier = if mainnet { Environment::Live } else { Environment::Demo };
    // ⚠ `..._for_account`, not `..._from`, and for the DEFAULT account the two are the same call:
    // `vike_model::account_keys::account_key` returns its input UNCHANGED for `AccountLabel::Default`,
    // so a single-account store's key names are not merely compatible, they are the same strings.
    let creds = load_credentials_for_account(venue, tier, account, vars);
    // THE HALF-CREDENTIAL REPORT, and the ONE site that makes it audible. A venue whose signer
    // REQUIRES a passphrase (`vike_bridge_core::venue_passphrase`'s `venue_passphrase` — OKX today)
    // and whose store holds only key+secret resolves `None` above, so it stays PAPER exactly like
    // an unconfigured venue — the correct outcome, and an INVISIBLE one: the operator wrote real
    // credentials and would see a silent paper mount with no error anywhere. So we name the missing
    // variable here.
    //
    // `error!`, not `warn!`: this is the same class as an unreadable store — credentials that EXIST
    // and cannot be used — which `credentials::load_workspace_secrets_at` also reports at ERROR,
    // and for the same reason (a misconfiguration wearing the "not configured" answer looks exactly
    // like a correct fresh install). The MainnetNoCreds line below stays `warn!` because nothing
    // there is half-written: the operator armed a flag and supplied no live key set at all.
    //
    // ⚠ HERE and not in the loader: `load_credentials_from` is called per-venue per-tier on hot
    // paths — `vike_connections::credential_status` re-runs the whole grid EVERY FRAME the GUI's
    // Connections tool is open — so a log line inside it would write at frame rate into a file
    // layer that defaults to `trace`. `missing_required_passphrase` is PURE and returns the finding
    // as data (the `vike-secrets` permission-warning shape); `make_engine` runs once per venue per
    // session, which is exactly how often an operator needs to be told. The NAME is logged; no
    // credential value ever is.
    if let Some(missing) =
        vike_bridge_core::credentials::missing_required_passphrase(venue, tier, vars)
    {
        tracing::error!(
            venue,
            "{missing} is unset or blank, but {venue} REQUIRES an API passphrase — its key and \
             secret alone cannot sign a single request. These credentials are UNUSABLE, so {venue} \
             stays PAPER (absent credentials are the live gate). Set {missing} in \
             <project>/settings/secrets.env to mount it live."
        );
    }
    if cex_cred_choice(mainnet, creds.is_some()) == CexCredChoice::MainnetNoCreds {
        tracing::warn!(
            venue,
            "{}_MAINNET=1 but no LIVE credentials present → staying PAPER (mainnet requires the \
             venue's LIVE key set; absent creds are the live gate, so a mainnet host is never signed \
             with demo keys)",
            venue.to_uppercase()
        );
    }
    // PRE-CONNECT REFUSAL — closing the #817 "the refusal happens POST-connect" residual
    // (Freqtrade refuses before touching a broker; until this check, the venue session was
    // already established when the budget check further down failed). The decision is PURE:
    // `would_mount_live` consults the SAME config loaders the arms below gate on (no network),
    // and the budget preview is exact — the two account-dependent caps
    // `require_live_risk_budget` reads are OPERATOR-owned fields the post-arm merge takes from
    // the profile on EVERY path (`ProfileRisk::apply_to` AND the `apply_operator_budget_only`
    // fallback alike), and no venue fetch or armed default ever sets them, so the preview
    // verdict always equals the post-merge verdict. The post-merge check stays as the BACKSTOP
    // for a future live arm this probe does not know about (probe-drift defense) — on today's
    // roster it is unreachable, because this fires first.
    //
    // INTENT-based on purpose: a venue whose synchronous connect would later fail and demote to
    // paper (ctrader/ibkr), or whose live factory declines a present-but-bad key (hyperliquid/
    // polymarket), still refuses HERE — present live config IS the operator's declared intent to
    // trade live, and an intended-live session must carry a bounded budget before anything
    // touches a venue.
    //
    // ⚠ CEILING-AWARE (`would_mount_live_under`, not `would_mount_live`): the probe has to answer
    // the question THIS mount will ask, and the ceiling changes it. Under `demo` with
    // `BINANCE_MAINNET=1` the uncapped probe reads the LIVE key set while the arm below reads the
    // DEMO one, and the two disagree in BOTH directions — a live-only store would refuse a mount
    // that can only be paper, and a demo-only store would escape the budget refusal on a mount that
    // genuinely arms. The second of those is the one that matters: an armed venue with no bounded
    // budget is exactly what this refusal exists to stop.
    //
    // ⚠ …and per ACCOUNT, for the same reason it is ceiling-aware: the probe has to answer the
    // question THIS mount will ask. Reading the DEFAULT account's credentials for a labelled mount
    // disagrees in both directions — a box whose second account alone is configured would escape
    // the budget refusal on a mount that genuinely arms, which is the direction that matters. For
    // `AccountLabel::Default` this is `would_mount_live_under` unchanged.
    if account_arming_under(venue, account, vars, mode).0 != vike_config::VenueMode::Paper {
        require_live_risk_budget(
            venue,
            &risk_profile.map(vike_exec::ProfileRisk::to_risk_limits).unwrap_or_default(),
            risk_profile.is_some(),
        )?;
    }
    // Static published fee schedule, evaluated ONCE (fee model follow-up 1): the paper arms below
    // and the post-match `resolve_fee_schedule` both used to call `fee_schedule_for(venue)`
    // independently — the reviewer's double-eval. Hoisting it here is the single source: the paper
    // arms fill their book with it, and `resolve_fee_schedule` prefers a live rate over it.
    //
    // KEYED BY LANE, not by the bare venue string. binance and aster each mount SPOT and USDⓈ-M
    // PERP behind one venue id, chosen by a trailing `.P` on `symbol` (their `exec::run` does the
    // same `split_symbol` and dispatches `run_perp`/`run_spot`), and binance prices the two lanes 5x
    // apart on the maker side. This site evaluated `fee_schedule_for(venue)` — one lookup, no lane —
    // and threaded the result into every paper fallback below (they all go through `paper_client`), so
    // every `BTCUSDT.P` paper/backtest mount filled its book at Binance SPOT fees. `fee_lane` is the
    // identity for every other venue and for every non-`.P` symbol, so this is byte-identical
    // everywhere except the lane it exists to split.
    let static_default = vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    let mut limits = vike_exec::RiskLimits::new();
    // The mounted symbol's contract size / notional multiplier, harvested from the SAME best-effort
    // instrument pre-fetch that builds `limits` above and folded into the engine's `Account`
    // multiplier grid at the single `Account::new` site below.
    //
    // WHY this exists: `validate_with_multiplier` computes an order's true notional as
    // `|qty| * |price| * multiplier`, and the GUI reads the multiplier off `CoreSnapshot`. Both
    // resolve through `Account::multiplier_of`, which — until this was wired — read an EMPTY grid
    // with a 1.0 default for all 14 venues. A Deribit option (1 BTC per contract) therefore
    // measured its notional as if one contract were one dollar of underlying, and the order-entry
    // notional cap passed orders it should have blocked.
    //
    // `0.0` = the venue reported no contract size, which `SymbolProperties::multiplier` folds to
    // the inert `1.0` — so every venue that does not populate it is byte-identical to today.
    let mut contract_size = 0.0f64;
    // The mounted symbol's RULING margin mode, harvested from the same live-arm instrument fetch
    // and folded into the engine's `Account` at the single `Account::new` site below (see
    // [`margin_mode_grid`]). `Cross` is both the type default and the right answer for 13 of the 14
    // roster venues, so every arm that does not write it stays byte-identical.
    //
    // WHY this exists: `Account::fold` books a position opened FROM FLAT with whatever margin mode
    // it is told, and until this was wired it was told nothing — `PositionEntry::default()`, i.e.
    // `Cross`, for every venue and every asset. Hyperliquid publishes `onlyIsolated` PER ASSET and
    // on such an asset the mode that rules is `Isolated` (#1087). Nothing downstream repairs the
    // mistake: HL reconciles through the `recon::diff`/`resolve` lane, which never reads
    // `PositionStatusReport::margin_mode`, and it has no `ReconcileSnapshot` producer at all — so
    // `ExecutionEngine::apply_snapshot`'s `position_margin` overwrite, the ONE place venue truth
    // wins this field, never runs for it. Booking it right at the fold is the only fix.
    //
    // ⚠ KNOWN SIBLING, DELIBERATELY NOT FIXED HERE: polymarket is the one roster venue whose
    // `caps_for(venue).default_margin_mode` is NOT `Cross` — it is `MarginMode::Cash`, the fully
    // collateralized mode — so a polymarket position folded from a fill is mis-booked the same way.
    // Seeding this local from `caps_for(venue).default_margin_mode` instead of the literal would
    // close it in one word, and that is exactly why it is NOT done in passing: `Cash` is excluded
    // from `margin_in_use_by`'s `is_cross()` filter, so the change alters what the admitting gate
    // charges on that venue, and it makes `PositionEntry.margin_mode` serialize (it only skips on
    // `Cross`) for every polymarket position — a `state_hash` surface change. That belongs to a PR
    // that can verify the venue, not to this one. Note `make_engine` mounts no polymarket EXEC
    // client today (its arm is recon-only), so nothing folds a fill through this path yet.
    let mut default_margin_mode = vike_model::MarginMode::Cross;
    // The per-symbol PRICE/SIZE GRID for the mount's DECLARED LEGS (`declared_legs`), folded onto
    // `limits.grid_by_symbol` at the single site below the match. Filled by the arms that can
    // resolve a non-mounted symbol's grid from something they ALREADY hold; left EMPTY by every
    // other arm, which is byte-identical to every mount before this existed — see
    // [`symbol_grid`]'s module doc for the rule and [`declared_grid_source`] for the per-venue
    // declaration.
    //
    // ⚠ It CANNOT be written straight into `limits` from inside an arm: the arms assign
    // `limits = RiskLimits::from_properties(&f)` WHOLESALE, so a map written before that line is
    // silently discarded and one written after would have to be repeated in every arm. A separate
    // local folded in once is the shape `contract_size`/`default_margin_mode` above already use,
    // for the same reason.
    let mut symbol_grids: indexmap::IndexMap<String, vike_exec::SymbolGrid> =
        indexmap::IndexMap::new();
    // Set inside a live arm below: crypto-CEX arms call `build_recon_client`; the deribit / aster /
    // hyperliquid / ctrader arms build their `ReconClient` inline (each needs a non-REST or
    // bespoke-signer handshake `build_recon_client` can't produce). Stays `None` for paper venues. A side-settable local
    // (rather than a second match-arm return value) so `client`'s match keeps its original single-
    // type shape instead of a second tuple-typed annotation.
    let mut recon: Option<Box<dyn vike_exec::recon::ReconClient>> = None;
    // API-KEY PERMISSION GATE (api-key-permissions capability map, STEP-2 — the wiring its STEP-1
    // reported as owed). Resolved BEFORE the match, not inside a match guard, so the one blocking
    // signed read it can issue is visible at the top level rather than hidden in a pattern. Only a
    // credentialed binance MAINNET mount probes anything (see `binance_withdraw_gate`); every other
    // (venue, creds) pair short-circuits to `Allow` with no network and no behavior change. A
    // `Refuse` makes the `("binance", Some(c))` arm below not match, so the venue falls through to
    // the paper `_` arm — never marked live, never spawned — exactly as absent credentials do.
    let binance_key_gate = match (venue, creds.as_ref()) {
        ("binance", Some(c)) => binance_withdraw_gate(mainnet, c),
        _ => WithdrawGate::Allow,
    };
    let client: Box<dyn vike_exec::ExecutionClient + Send> = match (venue, creds) {
        ("bybit", Some(c)) => {
            live_venues.insert(route_key.clone());
            if mainnet {
                tracing::warn!(
                    "⚠ REAL-MONEY: bybit mounting on MAINNET with LIVE credentials (real funds)"
                );
            } else {
                tracing::warn!(
                    "bybit: DEMO credentials present → LIVE exec client (real demo orders)"
                );
            }
            // PIT filter recording (opt-in, off by default): `properties_rec` is the caller-built
            // `PropertiesRecorder::open_from_env` handle — `None` unless VIKE_RECORD_PROPERTIES ==
            // "1" (and the binary carries the store backend), so the disabled path is byte-identical.
            // Moved into this arm's spawn below (only one venue arm ever runs per call).
            // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (a duplicate
            // of the adapter's own in-thread fetch — acceptable, startup-only). On failure, fall
            // back to the permissive default (byte-identical to pre-PR-2b behavior).
            if let Some(f) = vike_bybit::fetch_bybit_properties(&c, symbol, mainnet) {
                limits = vike_exec::RiskLimits::from_properties(&f);
            }
            // Reconcile handle (audit A1 item 4): built from the SAME `c` before it moves into
            // `spawn_with_recorder` below (`build_recon_client` only borrows it). The trailing 0.0
            // is `okx_ct_val`, unused outside the "okx" arm of `build_recon_client`.
            recon = recon::build_recon_client(venue, symbol, &c, 0.0, mainnet);
            // Fee-attribution FD-broker code (unified cross-venue attribution, task 5), resolved
            // ONCE from the workspace `.env`: `BYBIT_BROKER_CODE`/`BYBIT_BUILDER_CODE`, validated
            // against bybit's `AttributionMechanic::Header { name: "X-Referer" }`. Absent/invalid
            // degrades to `None` — no `X-Referer` header at all, byte-identical to before this
            // existed.
            let bybit_broker_id =
                vike_bridge_core::credentials::attribution_code_from(vars, "bybit");
            // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — the same
            // "resolve at the mount, thread the value, never re-read from a spawned thread" idiom
            // `mainnet` and the attribution codes already follow. This arm used to POST a hardcoded
            // 2x to `/v5/position/set-leverage` while the RiskGate sized every order against
            // `[risk] max_leverage` (`im = 1.0 / max_leverage`), so the account and the gate ran on
            // two different numbers. An UNSET `max_leverage` still yields the historical 2.0 — see
            // `vike_bybit::exec::leverage_for` / `vike_bridge_core::leverage`. This is the
            // operator's REQUEST: the exec thread then clamps it to `leverageFilter.maxLeverage`
            // for the mounted symbol, read off the `instruments-info` response it already fetches
            // (`clamp_to_venue_cap`) — a cap it cannot read clamps nothing.
            let bybit_leverage = vike_bybit::exec::leverage_for(risk_profile);
            Box::new(vike_bybit::BybitExecutionClient::spawn_with_recorder(
                c,
                symbol.to_string(),
                fallback::bybit_fallback_properties(),
                live_events.clone(),
                properties_rec,
                recon_trigger,
                bybit_broker_id,
                mainnet,
                bybit_leverage,
            ))
        }
        ("okx", Some(c)) => {
            live_venues.insert(route_key.clone());
            if mainnet {
                tracing::warn!(
                    "⚠ REAL-MONEY: okx mounting on MAINNET with LIVE credentials (real funds)"
                );
            } else {
                tracing::warn!(
                    "okx: DEMO credentials present → LIVE exec client (real demo orders)"
                );
            }
            // PIT filter recording (opt-in): see the bybit arm above (caller-built handle,
            // byte-identical disabled path).
            // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (keyless,
            // duplicate of the adapter's own in-thread fetch — acceptable, startup-only). On
            // failure, fall back to the permissive default (byte-identical to pre-PR-2b behavior).
            // The SAME fetch also yields `ct_val` (contracts->base) — captured and reused below for
            // the reconcile client instead of fetching it a second time.
            let okx_ct_val = match vike_okx::fetch_okx_instrument(symbol, mainnet) {
                Some((f, ct_val)) => {
                    // ⚠ BASE units, not the raw grid. OKX's `lotSz`/`minSz`/`maxMktSz` count
                    // CONTRACTS — that is what `OkxInstrument::to_contracts` floors a base qty on,
                    // and what every wire-side consumer needs. But `OrderRequest.qty` is BASE, and
                    // so is everything the gate reasons about, so handing it the contracts grid
                    // makes it floor base quantities on a step `ct_val`-times too coarse: for
                    // BTC-USDT-SWAP (`ct_val` 0.01) that is 100x, and a legitimate 0.015 BTC order
                    // floors to 0.01 while a size the venue accepts is refused outright.
                    //
                    // `ct_val` was already in hand on this very line — returned by the same fetch
                    // and used below for the reconcile client — which is what made the omission
                    // invisible. The conversion lives in ONE place next to `to_contracts` so the
                    // unit law is not re-derived here.
                    limits = vike_exec::RiskLimits::from_properties(
                        &vike_okx::perp::properties_in_base(&f, ct_val),
                    );
                    ct_val
                }
                None => fallback::OKX_FALLBACK_CTVAL,
            };
            // Reconcile handle (audit A1 item 4): built from the SAME `c` before it moves into
            // `spawn_with_recorder` below (`build_recon_client` only borrows it).
            recon = recon::build_recon_client(venue, symbol, &c, okx_ct_val, mainnet);
            // Fee-attribution FD-broker code (unified cross-venue attribution, task 4), resolved
            // ONCE from the workspace `.env`: `OKX_BROKER_CODE`/`OKX_BUILDER_CODE`, validated
            // against okx's `AttributionMechanic`. Absent/invalid degrades to `None` — the wire
            // body then carries no `tag` key at all, byte-identical to before this existed.
            let okx_broker_code = vike_bridge_core::credentials::attribution_code_from(vars, "okx");
            // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — see the
            // bybit arm above for the full rationale (this arm POSTed the same hardcoded 2x to
            // `/api/v5/account/set-leverage`). UNSET ⇒ the historical 2.0, byte-identical. As on
            // bybit this is the operator's REQUEST — the exec thread clamps it to the instrument's
            // `lever` (its published max), read off the `public/instruments` response it already
            // fetches for `ct_val`.
            let okx_leverage = vike_okx::exec::leverage_for(risk_profile);
            Box::new(vike_okx::OkxExecutionClient::spawn_with_recorder(
                c,
                symbol.to_string(),
                fallback::okx_fallback_properties(),
                fallback::OKX_FALLBACK_CTVAL,
                live_events.clone(),
                properties_rec,
                recon_trigger,
                okx_broker_code,
                mainnet,
                okx_leverage,
            ))
        }
        // The guard is the api-key-permission refusal resolved above: `Refuse` (a KNOWN
        // withdraw-capable key, no operator override) makes this arm not match, so binance falls
        // through to the paper `_` arm. `Allow` — every demo mount, every Unknown, every trade-only
        // key — matches exactly as before, so the default path is byte-identical.
        ("binance", Some(c)) if binance_key_gate == WithdrawGate::Allow => {
            live_venues.insert(route_key.clone());
            // #771 + go-live: an armed `BINANCE_MAINNET` selected the LIVE creds `c` above; the
            // SAME verdict becomes the effective `Environment` here — through the adapter's own
            // pure `resolve_env_from` core, so the demo→mainnet upgrade rule lives in exactly one
            // place — and is threaded into BOTH the grid pre-fetch and the exec client, which bind
            // the hosts those live creds authenticate against. STEP 2: the adapter no longer
            // re-reads the flag from its exec thread, so this `env` is authoritative end-to-end;
            // unset ⇒ `Demo`, byte-identical to before.
            let env = vike_binance::exec::resolve_env_from(Environment::Demo, mainnet);
            if mainnet {
                tracing::warn!(
                    "⚠ REAL-MONEY: binance mounting on MAINNET with LIVE credentials (real funds)"
                );
            } else {
                tracing::warn!(
                    "binance: DEMO credentials present → LIVE exec client (real demo orders)"
                );
            }
            // PIT filter recording (opt-in): see the bybit arm above (caller-built handle,
            // byte-identical disabled path).
            // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (spot
            // /api/v3 or fapi /fapi/v1 exchangeInfo, routed inside by the `.P` suffix — a duplicate
            // of the adapter's own in-thread fetch, acceptable startup-only). On failure, fall back
            // to the permissive default (byte-identical to the pre-live behavior).
            if let Some((f, _base)) = vike_binance::fetch_binance_properties(env, &c, symbol) {
                limits = vike_exec::RiskLimits::from_properties(&f);
            }
            // Reconcile handle (audit A1 item 4): built from the SAME `c` before it moves into
            // `spawn_with_recorder` below (`build_recon_client` only borrows it); routes spot vs
            // perp by the SAME `.P` suffix `exec.rs::split_symbol` uses. The trailing 0.0 is
            // `okx_ct_val`, unused outside the "okx" arm of `build_recon_client`. The recon client
            // takes the SAME resolved `mainnet` verdict, so it reconciles the same (mainnet or
            // demo) account these `c` creds authenticate against.
            recon = recon::build_recon_client(venue, symbol, &c, 0.0, mainnet);
            // Fee-attribution Broker/Link id (unified cross-venue attribution, task 6), resolved ONCE
            // from the workspace `.env`: `BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE`, validated
            // against binance's `AttributionMechanic`. Absent/invalid degrades to `None` — the wire
            // body's `newClientOrderId`/`origClientOrderId` then carry the bare local coid, byte-
            // identical to before this existed.
            let binance_link_id =
                vike_bridge_core::credentials::attribution_code_from(vars, "binance");
            // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — see the
            // bybit arm above for the full rationale (this arm POSTed the same hardcoded 2x to
            // `/fapi/v1/leverage`). UNSET ⇒ the historical 2.0, byte-identical. Inert on a SPOT
            // symbol: only the `.P` perp driver has a leverage endpoint to post to. ⚠ UNCLAMPED,
            // unlike bybit/okx: binance publishes no leverage ceiling on `exchangeInfo` (verified
            // live) — its cap lives in the SIGNED, risk-tiered `/fapi/v1/leverageBracket`, so a
            // too-high request is refused by the VENUE instead. See `leverage_for`'s doc.
            let binance_leverage = vike_binance::exec::leverage_for(risk_profile);
            Box::new(vike_binance::BinanceExecutionClient::spawn_with_recorder(
                env,
                c,
                symbol.to_string(),
                fallback::binance_fallback_properties(),
                live_events.clone(),
                properties_rec,
                recon_trigger,
                binance_link_id,
                binance_leverage,
            ))
        }
        ("deribit", Some(c)) => {
            live_venues.insert(route_key.clone());
            tracing::warn!(
                "deribit: DEMO credentials present → LIVE exec client (real demo orders)"
            );
            // PIT filter recording (opt-in): see the bybit arm above (caller-built handle,
            // byte-identical disabled path).
            // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (keyless
            // `public/get_instrument`, a duplicate of the adapter's own in-thread fetch — acceptable,
            // startup-only). On failure, fall back to the permissive default (byte-identical to the
            // pre-live behavior).
            if let Some(f) = vike_deribit::fetch_deribit_properties(&c, symbol) {
                limits = vike_exec::RiskLimits::from_properties(&f);
                // Deribit is THE motivating venue: options/futures are quoted per CONTRACT, so
                // `contract_size` (a real `public/get_instrument` field) is what turns a size into a
                // notional. This is the one arm that populates a non-1.0 multiplier today.
                contract_size = f.contract_size;
            }
            // Reconcile handle (audit A1 item 4): `vike_deribit::recon_client` opens its OWN
            // dedicated authed order-WS (a blocking JSON-RPC auth), so reconcile report fetches never
            // contend the exec side's order-transport `Mutex` — an order submit is never delayed
            // behind a reconcile round-trip. Built from `&c` HERE, before `c` moves into
            // `spawn_with_recorder` below (mirrors the aster arm). `None` (auth/connect fail) stays
            // reconcile-inert for deribit, exec unaffected. This is why `build_recon_client` returns
            // `None` for deribit — its recon needs a blocking WS auth, unlike the pure REST venues.
            //
            // LAZY (the `recon_enabled` gate): that blocking authed WS handshake is skipped entirely
            // when reconciliation is off — the factory is not called, so a `VIKE_RECONCILE`-unset
            // mount does no authenticated network work here. Enabled ⇒ the same call as before.
            recon = recon_if_enabled(recon_enabled, || vike_deribit::recon_client(&c, symbol));
            Box::new(vike_deribit::DeribitExecutionClient::spawn_with_recorder(
                c,
                symbol.to_string(),
                fallback::deribit_fallback_properties(),
                live_events.clone(),
                properties_rec,
            ))
        }
        // Aster uses agent-wallet creds (ASTER_{LIVE|TESTNET}_{USER,PRIVATE_KEY,SIGNER}), NOT the
        // generic `{VENUE}_DEMO_*` shape, so `load_credentials_from` above returns `None` for it —
        // resolve them directly. Prefer LIVE (mainnet) then TESTNET: the mainnet-verified account is
        // `ASTER_LIVE_*`. ⚠ REAL MONEY: `ASTER_LIVE_*` present ⇒ this spawns a LIVE MAINNET exec
        // client. The verified account is currently unfunded, so orders reject until deposit, but
        // this IS the live gate for Aster (absent-credentials-is-the-live-gate, mainnet-first).
        ("aster", _) => {
            // ⚠⚠ THE MEASURED HOLE, and the one line that closes it. This chain tried
            // `Environment::Live` FIRST and there is NO flag to refuse it — aster is declared
            // SWITCHLESS in `vike_bridge_core::mainnet::mainnet_switch_for`, so no `ASTER_MAINNET`
            // exists and `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` structurally cannot carry a
            // row for it. An `ASTER_LIVE_*` pair sitting in `secrets.env` was therefore, by itself,
            // an authenticated MAINNET session on a daemon that never asked for one.
            //
            // The ceiling is the refusal that did not exist: under anything below `live` the Live
            // attempt is DELETED from the chain — not attempted-and-discarded, not attempted with
            // the result ignored. Under `live` the chain is byte-identical to before.
            //
            // ⚠ `..._for_account`, not `..._from`: this venue is MAINNET in practice, so a labelled
            // account reading the DEFAULT account's `ASTER_LIVE_PRIVATE_KEY` would be a second live
            // client signing for the first account's real-money wallet. For `AccountLabel::Default`
            // the two calls read the same key names.
            let resolved = live_permitted
                .then(|| {
                    vike_aster::signing::load_aster_credentials_for_account(
                        Environment::Live,
                        account,
                        vars,
                    )
                    .map(|c| (Environment::Live, c))
                })
                .flatten()
                .or_else(|| {
                    vike_aster::signing::load_aster_credentials_for_account(
                        Environment::Demo,
                        account,
                        vars,
                    )
                    .map(|c| (Environment::Demo, c))
                });
            if let Some((env, c)) = resolved {
                live_venues.insert(route_key.clone());
                tracing::warn!(
                    "aster: {} credentials present → LIVE exec client ({})",
                    if env == Environment::Live { "LIVE (mainnet)" } else { "TESTNET" },
                    if env == Environment::Live { "REAL MONEY" } else { "testnet orders" }
                );
                // PIT filter recording (opt-in): see the bybit arm above (caller-built handle,
                // byte-identical disabled path).
                // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (PUBLIC
                // exchangeInfo, spot /api/v3 or fapi /fapi/v1 routed inside by the `.P` suffix — a
                // duplicate of the adapter's own in-thread fetch, acceptable startup-only). On
                // failure, fall back to the permissive default (byte-identical to the pre-live
                // behavior).
                if let Some((f, _base)) = vike_aster::fetch_aster_properties(env, &c, symbol) {
                    limits = vike_exec::RiskLimits::from_properties(&f);
                }
                // Reconcile handle (audit A1 item 4): `vike_aster::recon_client`, built from the
                // RESOLVED env + agent creds (µs EIP-712 signer), routed spot vs perp by the same
                // `.P` suffix. Called here (not via the env-less `build_recon_client`) so it hits
                // the SAME network the exec client does; borrows `&c` before it moves into spawn
                // below.
                recon = vike_aster::recon_client(env, &c, symbol);
                // Aster Code (unified cross-venue attribution, task 8): resolved ONCE from the
                // workspace `.env`. `attribution_code_from` validates the address against
                // `aster`'s `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None`
                // — byte-identical (no `builder`/`feeRate` on the wire at all, matching every
                // mount before this field existed). The fee defaults to `"0"` (attribution-only, no
                // `approveBuilder` grant needed — see `vike_aster::perp::AsterPerpRest::
                // approve_builder`'s doc) unless `ASTER_BUILDER_FEE_RATE` is set AND the operator
                // has separately run the one-time approval.
                let builder = vike_bridge_core::credentials::attribution_code_from(vars, "aster")
                    .map(|address| {
                        let fee_rate = vars
                            .get("ASTER_BUILDER_FEE_RATE")
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "0".to_string());
                        (address, fee_rate)
                    });
                // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — see
                // the bybit arm above for the full rationale (this arm POSTed the same hardcoded
                // 2x to `/fapi/v3/leverage`). UNSET ⇒ the historical 2.0, byte-identical. ⚠ THIS
                // venue resolves LIVE creds first and is MAINNET in practice, so the unchanged-
                // when-unset property is guarding a real-money account here. ⚠ UNCLAMPED, unlike
                // bybit/okx: Aster's fapi fork publishes no ceiling on `exchangeInfo` (verified
                // live) — its cap lives in the SIGNED, risk-tiered `/fapi/v3/leverageBracket`, so a
                // too-high request is refused by the VENUE instead. See `leverage_for`'s doc.
                let aster_leverage = vike_aster::exec::leverage_for(risk_profile);
                Box::new(vike_aster::AsterExecutionClient::spawn_with_recorder(
                    env,
                    c,
                    symbol.to_string(),
                    fallback::aster_fallback_properties(),
                    live_events.clone(),
                    properties_rec,
                    builder,
                    aster_leverage,
                ))
            } else {
                // No Aster creds ⇒ paper (identical to the `_` fallback below).
                Box::new(paper_client(venue, symbol, static_default))
            }
        }
        // Polymarket — the second FEATURE-GATED venue (this whole arm compiles only under
        // vike-mount's `polymarket` feature, which also turns on vike-polymarket's own `polymarket`
        // feature; without it that crate is EMPTY, so there is nothing to call and
        // `("polymarket", _)` falls through to the paper `_` arm — byte-identical to a default build).
        //
        // ⚠⚠ THE ONE VENUE WITH NO TESTNET. Every order this arm can place is REAL MONEY on Polygon
        // mainnet — there is no demo tier to mis-mount into. That is why exec is gated TWICE (creds
        // AND `POLY_EXEC=1`) where every other venue is gated once, and why the flag is checked
        // BEFORE the factory so an unset one makes no network call at all.
        //
        // Two INDEPENDENT opt-ins, composing four ways:
        // - `POLY_EXEC=1`      → `live_mount_from_vars` spawns the real `PolymarketExecutionClient`,
        //                        whose exec thread ALSO owns the authenticated user-WS fill pump and
        //                        its A3 resync (the return lane: on this venue every post-acceptance
        //                        terminal — fill, cancel, expiry — arrives only there). When
        //                        reconcile is also on, that ONE call returns the `ReconClient` too,
        //                        over the SAME L2 handshake and the SAME `PolymarketRegistry`, so
        //                        order reports finally re-key to local coids instead of `None`.
        // - `POLY_RECONCILE=1` → reconcile. With exec OFF it is the pre-existing recon-only mount
        //                        (`recon_client_from_vars`, fresh empty registry, exec PAPER).
        // - neither            → nothing is built, no network call, venue stays PAPER.
        //
        // Both flags read the process env OR the workspace `.env` map. `live_mount_from_vars`
        // resolves the L1 signer address (NOT the funder — ClobAuth must name the key's own EOA) and
        // derives the L2 trio over ONE blocking, proxy-routed round-trip at mount, exactly like the
        // deribit/ig/oanda/ctrader inline handshakes; ANY failure is `None` — the venue falls back to
        // paper, never a mount failure or a panic.
        //
        // ACCOUNT-WIDE, like the recon half: orders, fills, positions and balance all key off the
        // wallet, not `symbol`. `symbol` is used for exactly one thing — a best-effort pre-resolution
        // of that token's NegRisk signing domain, inert on failure. No `RiskLimits` pre-fetch /
        // `contract_size` applies (Polymarket has no instrument grid of that shape; shares are whole
        // and price is a probability). Interval-only reconcile: `recon_trigger` is not read, and
        // polymarket is not in the app's `recon_feed_statuses`, so its health gate reads Healthy.
        //
        // POLICY: `POLY_RECONCILE=1` WITHOUT `POLY_EXEC=1` diffs live venue state against a PAPER
        // engine, and wants `VIKE_RECONCILE_POLICY=quarantine`. The reason is `PositionDrift` — the
        // one kind `hybrid` genuinely auto-applies — which would fold the LIVE account's position
        // into the paper engine's books at the venue's avg price. With exec mounted, both sides are
        // the same account and `hybrid` is sound.
        //
        // ⚠ This comment used to say `hybrid` "would auto-cancel every paper order each pass" via
        // `OrphanLocalOrder`. That was false — the kind resolves to zero events under every policy
        // and `resolve` synthesizes no cancel at all. See
        // `vike_polymarket::recon_client`'s `## ⚠ ROLLOUT` doc and
        // `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
        // See `vike_polymarket::poly_reconcile_enabled` and `vike_polymarket::mount` — the authorities.
        #[cfg(feature = "polymarket")]
        ("polymarket", _) => {
            // BOTH gates, and the master one is not decoration here: it is what stops this arm
            // doing the venue's authenticated L2 round trip to build a handle the driver will never
            // be mounted to use. See [`poly_recon_wanted`] for the defect that spelling removes and
            // for what S2 changed for an operator who set `POLY_RECONCILE=1` and nothing else.
            let want_recon =
                poly_recon_wanted(recon_enabled, vike_polymarket::poly_reconcile_enabled(vars));
            // ⚠ THE CEILING AND `POLY_EXEC` ARE BOTH REQUIRED, and neither replaces the other.
            // `venues.polymarket = "live"` does NOT arm exec — the venue's own double gate stands
            // and `POLY_EXEC=1` is still mandatory. What the ceiling adds is the refusal in the
            // other direction, which is the one that was missing: `paper` overrides `POLY_EXEC=1`
            // outright, and it does so from ABOVE this arm (the early return at the top of this
            // function), so `poly_exec_enabled` — which reads the PROCESS environment first, where
            // a shell export beats anything in the credential store — is never even consulted. A
            // gate that ran after it could be argued with; one that runs before it cannot.
            //
            // `demo` is refused here rather than at that early return, because it is a
            // venue-SPECIFIC fact and this is the venue: Polymarket has NO testnet, so `demo` names
            // a tier that does not exist and the only tier the arm could honour is REAL MONEY on
            // Polygon mainnet. Arming it would be the ceiling widening a mount, which
            // `VenueMode::cap` exists to make impossible.
            //
            // ⚠ RESIDUAL, stated rather than implied: this gates EXEC. Under `demo` the RECON-ONLY
            // lane below still runs when `POLY_RECONCILE=1`, exactly as it does today — those are
            // authenticated mainnet READS, they place no order, and narrowing them is a separate
            // decision about what a ceiling governs. Under `paper` nothing in this arm runs at all.
            let exec_permitted = vike_polymarket::poly_exec_enabled(vars) && live_permitted;
            if vike_polymarket::poly_exec_enabled(vars) && !live_permitted {
                tracing::warn!(
                    venue,
                    ceiling = %mode,
                    "POLY_EXEC=1 is set, but this deployment's arming ceiling for polymarket is \
                     not `live` → exec stays PAPER. Polymarket has no testnet: `demo` names a tier \
                     that does not exist here, so the ceiling refuses rather than arming the only \
                     tier there is (real money on Polygon mainnet). Set venues.polymarket = \
                     \"live\" in <project>/settings/policy.toml to allow it."
                );
            }
            let mounted = if exec_permitted {
                // ⚠ `_for_account`: every credential AND the wallet's signature type come from THIS
                // account's key names. On a venue with no testnet that is the whole safety
                // property — a labelled account may never sign with the default wallet's key.
                vike_polymarket::live_mount_for_account(
                    vars,
                    account,
                    Some(symbol),
                    want_recon,
                    live_events,
                )
            } else {
                None
            };
            match mounted {
                Some(m) => {
                    // LIVE, REAL-MONEY MAINNET exec (the factory already warned, with the funder).
                    live_venues.insert(route_key.clone());
                    recon = m.recon;
                    if want_recon && recon.is_none() {
                        tracing::warn!(
                            "polymarket: exec mounted but no reconcile client could be built → \
                             reconcile-inert (exec unaffected)"
                        );
                    }
                    m.client
                }
                None => {
                    // Exec off, or it could not be built: absent creds, a bad key, an L2 derivation
                    // failure, or a REFUSED region pre-flight (the venue's own geoblock endpoint
                    // saying this egress may not place orders — logged at `error!` rather than
                    // `warn!`, because that one is a verdict rather than an absence). Each logged
                    // its own reason inside the factory. Fall back to the
                    // pre-existing RECON-ONLY behavior, which is the paper engine plus, when asked,
                    // a reconcile client over its own fresh registry.
                    //
                    // ⚠ That fallback stays CORRECT under a geoblock refusal, and deliberately so:
                    // the venue restricts order PLACEMENT only, so the reads a reconcile client
                    // makes are still legal from the refused region.
                    if want_recon {
                        recon = vike_polymarket::recon_client_for_account(vars, account);
                        if recon.is_some() {
                            tracing::warn!(
                                "polymarket: POLY_RECONCILE=1 without a live exec mount → \
                                 RECONCILE-ONLY (exec is PAPER). Run under \
                                 VIKE_RECONCILE_POLICY=quarantine: an auto-applying policy folds \
                                 PositionDrift, which would import the LIVE account's position \
                                 into the PAPER engine's books at the venue's avg price. The \
                                 startup line from vike_ops::reconcile_config names exactly what \
                                 the resolved policy auto-applies. See \
                                 vike_polymarket::poly_reconcile_enabled"
                            );
                        } else {
                            tracing::warn!(
                                "polymarket: POLY_RECONCILE=1 but no reconcile client could be \
                                 built → reconcile-inert"
                            );
                        }
                    }
                    Box::new(paper_client(venue, symbol, static_default))
                }
            }
        }
        // Hyperliquid's credentials are the bespoke private-key shape (loaded inside the helper via
        // `config::load`, not the standard `creds` above), so it matches on `_` creds and gates
        // itself. `spot`/`perp` both route here (one engine per venue); the perp `"BTC"` is mounted.
        // The helper also builds HL's `ReconClient` (bespoke signer/transport path, not
        // `build_recon_client`) and returns it alongside the exec client — wired into `recon` here.
        //
        // THE POLICY CONSUMER (Phase 6c): this is the one venue on the roster with no native market
        // order, so it is the one arm `MountPolicy::market_slippage` binds. `None` (no
        // `policy.toml`, or a policy that does not name the key) is byte-identical — the helper
        // resolves it through `vike_hyperliquid::exec::market_slippage_for`, whose unset path
        // returns the adapter's own historical literal.
        ("hyperliquid", _) => {
            match hyperliquid::hyperliquid_live_client(
                symbol,
                declared_legs,
                vars,
                account,
                live_events,
                &mut limits,
                &mut symbol_grids,
                &mut default_margin_mode,
                recon_trigger,
                policy.and_then(|p| p.market_slippage),
                // …and the SECOND policy field this arm consumes: the arming ceiling, which decides
                // whether `HYPERLIQUID_MAINNET=1` is allowed to select mainnet at all. Same
                // conjunct as `mainnet` above, at the one venue whose tier switch is resolved
                // inside the helper rather than here.
                live_permitted,
            ) {
                Some((client, recon_client)) => {
                    live_venues.insert(route_key.clone());
                    recon = Some(recon_client);
                    client
                }
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // cTrader uses OAuth token creds (CTRADER_CLIENT_ID/_SECRET + CTRADER_DEMO_ACCESS_TOKEN/
        // _REFRESH_TOKEN), NOT the generic `{VENUE}_DEMO_API_KEY` shape, so `load_credentials_from`
        // above returns `None` for it — it self-gates on `CtraderConfig::from_vars(Demo, …)` (like
        // the aster/hyperliquid arms match on `_`). This is the EXEC-ONLY mount: market data comes
        // from the separate `CtraderData` feed vike-app wires elsewhere.
        //
        // ⚠ WEAKER ROBUSTNESS CONTRACT (fast-follow, called out in the PR body): unlike every other
        // live arm, cTrader's `connect_and_auth_exec` is a BLOCKING, FALLIBLE protobuf/TLS handshake
        // performed synchronously HERE at mount time — the crypto/deribit/aster arms instead spawn an
        // actor that reconnects internally and never fails the mount. A connect failure therefore
        // DEMOTES cTrader to PAPER for the whole session (there is no in-thread reconnect). A
        // follow-up should move the connect into a self-healing spawn like the sibling adapters so a
        // transient startup outage doesn't strand ctrader on paper.
        ("ctrader", _) => {
            // ⚠ `from_vars_with_store`, not `from_vars`: the extra argument is the ROTATION HOME.
            // cTrader rotates its refresh token on every refresh, so a mount that cannot write the
            // new pair back leaves the credential store carrying an already-spent grant — and the
            // NEXT cold start then refuses (`CH_ACCESS_TOKEN_INVALID`) with a valid-looking pair on
            // disk. The state directory is the one the composition root's boot already resolved
            // (`declared_project_state_dir`); the store is DERIVED from it rather than re-walked,
            // because the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-blind and all three
            // shipped units set that variable. A root that declared nothing (a test, a tool) yields
            // `None` and the pre-rotation behaviour, byte-identically.
            // ⚠ `..._for_account`: the account's own grant AND — through `TokenKeys::for_account`
            // inside it — the account's own ROTATION TARGET, so a refresh here can never overwrite
            // another account's tokens in the shared store.
            let ctrader_cfg = vike_ctrader::config::CtraderConfig::from_vars_with_store_for_account(
                Environment::Demo,
                account,
                vars,
                vike_bridge_core::halt::declared_project_state_dir().as_deref(),
            );
            let client: Box<dyn vike_exec::ExecutionClient + Send> = match ctrader_cfg {
                Some(cfg) => {
                    // Inline recon FIRST, on its OWN dedicated authed socket (mirrors the deribit /
                    // aster arms): a reconcile report fetch never contends the exec actor's protobuf
                    // socket. `None` (handshake fail) stays reconcile-inert; exec unaffected. cTrader
                    // has NO PropertiesRecorder and takes NO `recon_trigger` (interval-only
                    // reconcile, like deribit/aster) — `recon_trigger` is intentionally not read.
                    //
                    // LAZY (the `recon_enabled` gate): with reconciliation off the protobuf/TLS
                    // OAuth handshake is never performed — the factory is not called. Enabled ⇒ the
                    // same call, in the same position (still BEFORE the exec connect below), so the
                    // connect-failure arm's `recon = None` demotion still covers it.
                    recon = recon_if_enabled(recon_enabled, || {
                        vike_ctrader::recon_client(&cfg, symbol)
                    });
                    // A no-op live-data sink: the exec actor's data lane fans out onto an empty
                    // `TeeSink` (no inner sinks) because this mount wires exec only.
                    let sink: std::sync::Arc<dyn vike_data::LiveDataSink> =
                        std::sync::Arc::new(vike_data::TeeSink(Vec::new()));
                    match vike_ctrader::conn::connect_and_auth_exec(
                        cfg.to_conn_config(),
                        sink,
                        live_events.clone(),
                    ) {
                        Ok(handle) => {
                            live_venues.insert(route_key.clone());
                            tracing::warn!(
                                "ctrader: DEMO credentials present → LIVE exec client (real demo orders)"
                            );
                            // Live RiskGate from the handshake-resolved symbol grid (PR-2b): no
                            // extra network — the grid was resolved during the exec handshake above.
                            // An unknown symbol keeps the permissive default (byte-identical to
                            // pre-PR-2b behavior), same as the crypto arms' failed pre-fetch.
                            if let Some(f) = handle.symbols.risk_properties(symbol) {
                                limits = vike_exec::RiskLimits::from_properties(&f);
                            }
                            // …and the SAME table answers for every DECLARED LEG, at no network
                            // cost: `risk_properties` reads the handshake's own `SymbolsList`
                            // (its doc says "Needs NO network"), so a second FX pair costs one
                            // map lookup. This is an arm `declared_grid_source`
                            // classifies `InHand`; an unknown leg name simply gets no row and is
                            // reported by `warn_ungridded_legs` below.
                            symbol_grids =
                                symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
                                    handle.symbols.risk_properties(leg)
                                });
                            // THE halt-admit consumer. cTrader is the ONE venue where
                            // `HaltAdmit::Verify` does anything (`vike_model::halt_verify_support`),
                            // because it is the only adapter holding a position book at its halt
                            // boundary — seeded at connect, kept fresh from every execution event.
                            // `Admit` (the default, and every deployment with no `policy.toml`) is
                            // byte-identical to the flag-trusting rule cTrader got in #1180.
                            Box::new(
                                vike_ctrader::exec::CtraderExec::new(handle, live_events.clone())
                                    .with_halt_admit(halt_admit),
                            )
                        }
                        Err(e) => {
                            // Demote to PAPER for this session (see the weaker-robustness note
                            // above). The recon socket built moments ago is now incoherent — it
                            // would reconcile a PAPER engine against LIVE cTrader state — so drop it:
                            // a paper venue holds no reconcile handle, exactly like the absent-creds
                            // path and every other paper venue.
                            recon = None;
                            tracing::warn!(
                                error = %e,
                                "ctrader: exec connect/auth failed → falling back to PAPER for this session"
                            );
                            Box::new(paper_client(venue, symbol, static_default))
                        }
                    }
                }
                // No cTrader creds ⇒ paper (byte-identical to the `_` fallback below; `recon`
                // stays `None`). This is the INERT-DEFAULT path proven for every roster venue by
                // `all_roster_venues_absent_creds_stay_paper_and_inert`.
                None => Box::new(paper_client(venue, symbol, static_default)),
            };
            // The ARMED/NOT-ARMED half of the halt-admit report, said HERE because here is the first
            // point in this function where the answer is known: `report_halt_admit` runs before
            // credentials and before the blocking handshake above, both of which can land this venue
            // on PAPER. See that function for why announcing `verify` from up there was a claim the
            // mount could not keep.
            report_halt_admit_armed(venue, halt_admit, live_venues.contains(&route_key));
            client
        }
        // Alpaca uses an OAuth2 client-credentials pair (ALPACA_SANDBOX_{CLIENT_ID,CLIENT_SECRET,
        // ACCOUNT_ID}), NOT the generic `{VENUE}_DEMO_API_KEY` shape, so `load_credentials_from`
        // above returns `None` for it — it self-gates on `load_alpaca_config_from(Demo, …)` (like the
        // aster/hyperliquid/ctrader arms match on `_`). This is the EXEC + inline-recon mount:
        // market data comes from the separate `AlpacaDataClient` feed vike-app wires elsewhere.
        //
        // Unlike ctrader, `AlpacaExecutionClient::spawn` is INFALLIBLE at mount time — it spawns a
        // self-reconnecting `ExecActor` + SSE reader (no blocking startup handshake), so there is NO
        // connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.
        ("alpaca", _) => {
            match vike_alpaca::load_alpaca_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    live_venues.insert(route_key.clone());
                    tracing::warn!(
                        "alpaca: SANDBOX credentials present → LIVE exec client (real sandbox orders)"
                    );
                    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking best-effort
                    // pre-fetch (`/v1/assets`, a throwaway OAuth2 lifecycle isolated from the exec/
                    // recon clients). On failure, fall back to the permissive default (byte-identical
                    // to pre-PR-2b behavior), same as the crypto arms' failed pre-fetch.
                    if let Some(f) = vike_alpaca::fetch_alpaca_properties(&cfg, symbol) {
                        limits = vike_exec::RiskLimits::from_properties(&f);
                    }
                    // Reconcile handle (audit A1 item 4): `vike_alpaca::recon_client` builds a FRESH
                    // Bearer `AlpacaRest` (a SECOND OAuth2 client-credentials lifecycle) dedicated to
                    // reconcile reads, isolated from the exec side's own — mirrors the deribit/aster/
                    // ctrader inline-recon arms. Built from `&cfg` HERE, before `cfg` moves into
                    // `spawn` below. Alpaca reconciles on the periodic INTERVAL only (no
                    // `recon_trigger`, like deribit/aster/ctrader), so that parameter is not read.
                    recon = vike_alpaca::recon_client(&cfg, symbol);
                    Box::new(vike_alpaca::AlpacaExecutionClient::spawn(cfg, live_events.clone()))
                }
                // No Alpaca creds ⇒ paper (byte-identical to the `_` fallback below; `recon` stays
                // `None`). This is the INERT-DEFAULT path proven for every roster venue by
                // `all_roster_venues_absent_creds_stay_paper_and_inert`.
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // IG (IG Group) FX/CFD via session-auth REST. LIVE exec when the `IG_DEMO_*` config
        // (`IG_DEMO_API_KEY`/`_IDENTIFIER`/`_PASSWORD`) is present, else paper (see the `_` fallback
        // below). Like alpaca/ctrader this is an EXEC-ONLY mount — market data comes from a separate
        // feed vike-app wires elsewhere; there is none in the app today, so an absent-creds IG is a
        // paper-INERT mount (no bar feed ⇒ the paper client never fills). IG uses the `IG_DEMO_*`
        // config shape, NOT the generic `{VENUE}_DEMO_API_KEY` one, so `load_credentials_from` above
        // returns `None` for it — the arm self-gates on `load_ig_config_from(Demo, …)` (like the
        // aster/hyperliquid/ctrader/alpaca arms match on `_`). `IgExecutionClient::spawn` is INFALLIBLE
        // at mount time (an ExecActor + a background Lightstreamer stream that each self-gate on their
        // own login — no blocking startup handshake, so there is NO connect-failure paper-demotion
        // branch, unlike ctrader/ibkr): present creds ⇒ live, absent ⇒ paper.
        ("ig", _) => {
            match vike_ig::load_ig_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    live_venues.insert(route_key.clone());
                    tracing::warn!(
                        "ig: DEMO credentials present → LIVE exec client (real demo orders)"
                    );
                    // Reconcile handle (recon breadth → IG): `vike_ig::recon_client` logs in a FRESH
                    // dedicated `IgSession`, isolated from BOTH the exec side's own session and its
                    // background Lightstreamer login (IG permits concurrent sessions). Built from
                    // `&cfg` HERE, before `cfg` moves into `spawn` below; the login is a blocking
                    // network round-trip at mount time — the SAME "build recon inline" discipline
                    // deribit/ctrader already follow. `None` (login fail) stays reconcile-inert; exec
                    // unaffected. IG reconciles on the periodic INTERVAL only (no `recon_trigger`, like
                    // deribit/aster/alpaca/ctrader), so that parameter is intentionally not read — and
                    // IG is NOT in `recon_feed_statuses` (no market_feed), so its health gate reads
                    // Healthy and is never blocked, like deribit/alpaca/ctrader.
                    //
                    // ORDER-COMPLETENESS (recon safety): `fetch_order_status_reports` reads
                    // `/workingorders` — the venue's CURRENT resting set — so a local order absent from
                    // it is a genuine terminal, never a truncation. Operators should still start IG
                    // under `VIKE_RECONCILE_POLICY=quarantine` until the read-only reconcile smoke
                    // has live-proven report completeness (see the PR body) — the exposure is
                    // POSITION-side: `hybrid` auto-applies `PositionDrift`. (⚠ this note used to
                    // say "under the default `hybrid` policy an OrphanLocalOrder auto-cancels";
                    // it does not — `crates/vike-exec/tests/recon/recon_policy_pin.rs`.)
                    //
                    // LAZY (the `recon_enabled` gate): with reconciliation off that dedicated
                    // `IgSession` login never happens — the factory is not called, so an IG mount
                    // under an unset `VIKE_RECONCILE` opens no second authenticated session.
                    recon = recon_if_enabled(recon_enabled, || vike_ig::recon_client(&cfg, symbol));
                    Box::new(vike_ig::IgExecutionClient::spawn(cfg, live_events.clone()))
                }
                // No IG creds ⇒ paper (byte-identical to the `_` fallback below; `recon` stays `None`).
                // The INERT-DEFAULT path proven for every roster venue by
                // `all_roster_venues_absent_creds_stay_paper_and_inert`.
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // OANDA v20 FX via Bearer REST. LIVE exec when the `OANDA_DEMO_*` config
        // (`OANDA_DEMO_API_KEY`/`_ACCOUNT_ID`) is present, else paper. EXEC-ONLY mount (no market_feed
        // in the app today ⇒ paper-inert without creds), self-gating on `vike_oanda::mountable_tier`
        // — the alpaca/ig arms' shape plus this venue's one refusal. `OandaExecutionClient::spawn` is
        // INFALLIBLE at mount time (an ExecActor + a self-reconnecting transactions-stream reader — no
        // blocking startup handshake, so no connect-failure demotion branch): practice creds ⇒ live,
        // absent ⇒ paper.
        //
        // ⚠ THE REFUSAL, and why this arm has one when its alpaca/ig siblings do not. OANDA's
        // `oanda_hosts` implements and tests the fxTrade tier, and NOTHING in the workspace ever asks
        // `load_oanda_config_from` for it — so before `mountable_tier` existed, an operator who wrote
        // real `OANDA_LIVE_*` keys into the store got a venue that stayed PAPER in silence, and one
        // who wrote BOTH tiers got REAL orders on the practice account while believing they were
        // live. That is the configured-and-inert shape the credential doctrine names outright: an
        // ABSENT credential is the ordinary unconfigured state and is silent, a PRESENT and unusable
        // one is an ERROR. So the arm refuses to select ANY tier from a live-armed store — the
        // practice fallback included — and says so at `error!`.
        //
        // WHY REFUSE RATHER THAN WIRE THE TIER. Arming fxTrade here would be a unilateral
        // real-money flip on a bridge whose own traps say it has no instrument grid at all (so
        // `RiskLimits` runs on the permissive default, see `symbol_grid::declared_grid_source`) and
        // formats every instrument at one fixed precision. The capability-map playbook flips
        // behaviour "one row at a time, behind demo smokes", and no smoke can exist for a tier with
        // no credentials anywhere; the sibling `("ibkr", _)` arm below already records the same
        // verdict for the same family ("a LIVE flip is a deliberate follow-up"). A flip also needs
        // the go-live machinery #771 built for the CEX venues — an explicit arming switch resolved
        // once per mount, the `⚠ REAL-MONEY` line, and the budget refusal keyed to it — none of
        // which a venue may grow on its own: `vike_bridge_core::mainnet::mainnet_switch_for` DECLARES
        // oanda switchless, and that is a shared capability table, extended by one coordinated PR
        // informed by every consumer, never edited in passing.
        //
        // `error!`, not `warn!`, and for the reason the passphrase report above states: this is the
        // class of an unreadable store — credentials that EXIST and cannot be used — and a
        // misconfiguration wearing the "not configured" answer looks exactly like a correct fresh
        // install. The NAMES are logged; the token never is (`UnreachableLiveTier` holds no value).
        ("oanda", _) => {
            match vike_oanda::mountable_tier_for_account(account, vars) {
                vike_oanda::MountableTier::Practice(cfg) => {
                    live_venues.insert(route_key.clone());
                    tracing::warn!(
                        "oanda: DEMO credentials present → LIVE exec client (real fxPractice orders)"
                    );
                    // Reconcile handle (recon breadth → OANDA): `vike_oanda::recon_client` opens its
                    // OWN dedicated Bearer `OandaRest` and probes `/summary` (validating the
                    // token+account and capturing the fill-floor watermark — a blocking network
                    // round-trip at mount time, like deribit/ctrader/ig), never the exec transport.
                    // Built from `&cfg` HERE, before `cfg` moves into `spawn`. `None` on connect failure
                    // stays reconcile-inert; exec unaffected. OANDA reconciles on the periodic INTERVAL
                    // only (no `recon_trigger`) and is NOT in `recon_feed_statuses` (no market_feed) →
                    // health gate reads Healthy, like deribit/alpaca/ctrader/ig.
                    //
                    // ORDER-COMPLETENESS (recon safety): `fetch_order_status_reports` reads
                    // `/orders?state=PENDING&count=500` — the current resting set — so a local order
                    // absent from it is a genuine terminal. Same quarantine-first rollout note as IG
                    // above (the exposure is `PositionDrift`, not an order-side auto-cancel).
                    //
                    // LAZY (the `recon_enabled` gate): with reconciliation off the dedicated Bearer
                    // `OandaRest` + its blocking `/summary` probe are never built — the factory is
                    // not called.
                    recon =
                        recon_if_enabled(recon_enabled, || vike_oanda::recon_client(&cfg, symbol));
                    Box::new(vike_oanda::OandaExecutionClient::spawn(cfg, live_events.clone()))
                }
                // THE REFUSAL (see the block above). Paper, `recon` stays `None` — the same inert
                // outcome an unconfigured venue reaches, arrived at LOUDLY instead of silently, and
                // reached even when `OANDA_DEMO_*` would have loaded.
                vike_oanda::MountableTier::LiveUnreachable(refusal) => {
                    tracing::error!(venue, "{refusal}");
                    Box::new(paper_client(venue, symbol, static_default))
                }
                // No OANDA creds ⇒ paper (byte-identical to the `_` fallback below; `recon` stays
                // `None`). The INERT-DEFAULT path proven for every roster venue by
                // `all_roster_venues_absent_creds_stay_paper_and_inert`.
                vike_oanda::MountableTier::Unconfigured => {
                    Box::new(paper_client(venue, symbol, static_default))
                }
            }
        }
        // IBKR is the ONE FEATURE-GATED live venue: its real socket/cpapi backends live behind
        // vike-ibkr's own `ibkr` feature (a default vike-ibkr build is a stub whose `connect` returns
        // `Unavailable`). This whole arm is therefore compiled only under vike-mount's own `ibkr`
        // feature — with it OFF, the arm does not exist and `("ibkr", _)` falls through to the paper
        // `_` arm below (byte-identical to today; the vendored ibapi tree is never compiled here).
        //
        // IBKR uses `IBKR_{DEMO|LIVE}_{HOST|PORT|CLIENT_ID|ACCOUNT|BACKEND}` config, NOT the generic
        // `{VENUE}_DEMO_API_KEY` shape, so `load_credentials_from` above returns `None` for it — the
        // arm self-gates on `load_ibkr_config_from(Demo, …)` (like the aster/hyperliquid/ctrader/
        // alpaca arms match on `_`). ABSENT ACCOUNT ⇒ paper (absent-credentials-is-the-live-gate). We
        // resolve the DEMO (paper-account) env only, matching the ctrader/alpaca arms; a LIVE flip is
        // a deliberate follow-up (resolve `IBKR_LIVE_*` first, mirroring the aster arm's REAL-MONEY
        // warning). The exec backend (socket default / cpapi) is chosen INSIDE
        // `IbkrExecutionClient::connect` by `cfg.backend`, resolved from `IBKR_DEMO_BACKEND`.
        //
        // ⚠ WEAKER ROBUSTNESS CONTRACT (same as ctrader): `IbkrExecutionClient::connect` is a
        // BLOCKING, FALLIBLE handshake performed synchronously HERE (socket: TCP connect + API
        // handshake to a running TWS/Gateway; cpapi: a browser-authenticated Client Portal Gateway).
        // A connect failure DEMOTES IBKR to PAPER for the whole session (there is no in-thread
        // reconnect), exactly like the ctrader arm.
        #[cfg(feature = "ibkr")]
        ("ibkr", _) => {
            match vike_ibkr::config::load_ibkr_config_for_account(Environment::Demo, account, vars)
            {
                Some(cfg) => {
                    // Inline recon FIRST, on its OWN dedicated cpapi `IbkrReconClient` (mirrors the
                    // deribit/aster/ctrader/alpaca arms): a reconcile report fetch never contends the
                    // exec transport. It is cpapi-only regardless of the exec backend, so with a
                    // socket exec backend and no CP Gateway up it resolves `None` (unwired) —
                    // graceful degradation, exec unaffected. IBKR reconciles on the periodic INTERVAL
                    // only (no `recon_trigger`, like deribit/aster/ctrader/alpaca), so that parameter
                    // is intentionally not read. Recon breadth: ibkr → reconciled venue #9 once the
                    // vike-app driver wiring lands.
                    //
                    // LAZY (the `recon_enabled` gate): with reconciliation off the cpapi
                    // `tickle`/`secdef_search` handshake is never performed — the factory is not
                    // called. Enabled ⇒ the same call, in the same position (still BEFORE the exec
                    // connect below), so the connect-failure arm's `recon = None` demotion still
                    // covers it.
                    recon = recon_if_enabled(recon_enabled, || {
                        vike_ibkr::recon_client::recon_client(&cfg, symbol)
                    });
                    match vike_ibkr::IbkrExecutionClient::connect(&cfg, live_events.clone()) {
                        Ok(client) => {
                            live_venues.insert(route_key.clone());
                            tracing::warn!(
                                backend = ?cfg.backend,
                                "ibkr: config present → LIVE exec client (real orders on the resolved Gateway account)"
                            );
                            // Live RiskGate from the venue's REAL grid: one blocking best-effort
                            // `contractDetails` pre-fetch (min_tick + size increments) over a
                            // dedicated throwaway socket connection. IBKR has NO keyless public grid
                            // endpoint like the crypto arms' exchangeInfo, so `fetch_ibkr_properties`
                            // opens its own transient connection (mirroring `HistoricalFetcher`) and
                            // drops it before the live feed comes up. On failure — unreachable
                            // Gateway, unparseable symbol, empty reply, OR the cpapi backend (this
                            // fetch is socket-only; the cpapi contract-info grid is a follow-up) — it
                            // returns `None` and `limits` keeps the permissive default, byte-identical
                            // to a failed pre-fetch on the other arms. The socket backend (default,
                            // live-verified) is the covered path.
                            if let Some(f) = vike_ibkr::fetch_ibkr_properties(&cfg, symbol) {
                                limits = vike_exec::RiskLimits::from_properties(&f);
                            }
                            Box::new(client)
                        }
                        Err(e) => {
                            // Demote to PAPER for this session (see the weaker-robustness note above).
                            // The recon client built moments ago would reconcile a PAPER engine
                            // against LIVE state — drop it, exactly like the ctrader connect-fail arm
                            // and every other paper venue.
                            recon = None;
                            tracing::warn!(
                                error = %e,
                                "ibkr: exec connect failed → falling back to PAPER for this session"
                            );
                            Box::new(paper_client(venue, symbol, static_default))
                        }
                    }
                }
                // No IBKR config (absent `IBKR_DEMO_ACCOUNT`) ⇒ paper (byte-identical to the `_`
                // fallback below; `recon` stays `None`). This is the INERT-DEFAULT path proven for
                // every roster venue by `all_roster_venues_absent_creds_stay_paper_and_inert` — which
                // also runs under the default (no-feature) build, where this arm is absent and the
                // same paper engine is produced by the `_` arm.
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // FXCM — the THIRD feature-gated venue, and the only one whose live gate is a property of
        // the BINARY rather than of the operator's credentials.
        //
        // `vike_fxcm::sdk_linked()` reports whether `build.rs` found the proprietary ForexConnect
        // SDK and emitted its `fcsdk` cfg. Without it every `FxcmSession` call returns
        // `Unavailable`, so `FxcmExecutionClient::spawn` still hands back a perfectly ordinary
        // exec client whose session thread RETURNS AT LOGIN — after which every submit is accepted
        // by the actor, forwarded to a thread that is not there, and discarded in silence. That is
        // the no-silent-vanish half of the venue-adapter contract failing at the mount, so this arm
        // refuses it OUT LOUD and lands on paper, the same shape as the oanda live-tier refusal.
        //
        // ⚠ Which means the CI configuration of this arm is the REFUSING one, on every runner and
        // in every feature lane: no CI machine has ever linked ForexConnect. What CI proves here is
        // that the arm compiles and refuses; that it TRADES is proven only on a box with the SDK
        // staged, by hand, through `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`.
        //
        // ⚠ WEAKER ROBUSTNESS CONTRACT, and weaker than ctrader/ibkr's: `spawn` is INFALLIBLE, so
        // unlike those two there is no connect result to demote on. A bad password, an unreachable
        // gateway and a stub build are indistinguishable from outside (`crates/bridges/fxcm/
        // CLAUDE.md` records this as by-design), so the stub case is the only one this arm can
        // catch — a live-but-failing login mounts "live" and trades nothing. Reconcile is what
        // notices; see the rollout note in the root CLAUDE.md.
        #[cfg(feature = "fxcm")]
        ("fxcm", _) => {
            match vike_fxcm::load_fxcm_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    if vike_fxcm::sdk_linked() {
                        // Inline recon on its OWN dedicated ForexConnect session (mirrors the
                        // deribit/ctrader/ig/oanda/ibkr arms): a reconcile table read never contends
                        // the exec session, which is single-threaded and blocking. FXCM reconciles on
                        // the periodic INTERVAL only (no `recon_trigger`), so that parameter is
                        // intentionally not read. LAZY behind `recon_enabled`: with reconciliation off
                        // the second login is never performed.
                        //
                        // ⚠ This is also the ONLY thing that recovers the fills a restart loses. The
                        // exec side routes an async fill to its client order id through an in-process
                        // map, so after a restart every re-surfaced trade is unroutable and dropped
                        // (`vike_fxcm::event_mapper::map_drained_event` says so at `warn!`).
                        // `FxcmReconClient::fetch_fill_reports` reads the same Trades table keyed by
                        // the same `trade_id`, so with `VIKE_RECONCILE=1` the gap closes on the next
                        // pass — and with reconciliation OFF it stays open. An operator mounting this
                        // venue accepts that; `vike_model::venue_caps`'s FXCM row states it.
                        recon = recon_if_enabled(recon_enabled, || {
                            vike_fxcm::recon_client(&cfg, symbol)
                        });
                        live_venues.insert(route_key.clone());
                        tracing::warn!(
                            connection = %cfg.connection,
                            "fxcm: config present and the ForexConnect SDK is linked → LIVE exec \
                             client (real orders on the resolved account)"
                        );
                        // No live RiskGate pre-fetch: FXCM exposes no symbol-properties endpoint short
                        // of a session-bound SDK call, so `limits` keeps the permissive default —
                        // byte-identical to a failed pre-fetch on the other arms.
                        Box::new(vike_fxcm::FxcmExecutionClient::spawn(cfg, live_events.clone()))
                    } else {
                        // THE REFUSAL. Paper, `recon` stays `None` — the same inert outcome an
                        // unconfigured venue reaches, arrived at LOUDLY instead of silently.
                        tracing::error!(
                            venue,
                            "fxcm: credentials are present but this binary has NO ForexConnect SDK \
                         linked (stub build), so a live mount would accept every order and \
                         discard it in silence. REFUSING the live mount and staying paper. Build \
                         with --features fxcm on a box where the SDK is staged (FCSDK_DIR, see \
                         crates/bridges/fxcm/scripts/provision-fcsdk.sh)"
                        );
                        Box::new(paper_client(venue, symbol, static_default))
                    }
                }
                // No FXCM credentials ⇒ paper (byte-identical to the `_` fallback below; `recon` stays
                // `None`). The INERT-DEFAULT path proven for every roster venue by
                // `all_roster_venues_absent_creds_stay_paper_and_inert`, which also runs under the
                // default build where this arm does not exist at all.
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // vike:new-venue:note do NOT add a `("{venue}", _)` arm yet. An absent arm falls through to the paper `_` arm below, which IS the correct state for a bridge with no live exec client — absent credentials are the live gate, and a half-wired arm would arm a venue nobody has smoke-tested. Add the arm in the PR that lands the live `ExecutionClient`, together with its demo smoke: crates/vike-mount/src/lib.rs's `make_engine_with_legs`
        // `recon` stays `None` (paper venues never get a reconcile handle).
        _ => Box::new(paper_client(venue, symbol, static_default)),
    };
    // The DECLARED-LEG grid, folded on at ONE site — after every arm's wholesale
    // `limits = RiskLimits::from_properties(&f)` (which would otherwise discard it) and before the
    // operator merge below (which carries `grid_by_symbol` through verbatim on BOTH of its paths,
    // `ProfileRisk::apply_to` and `apply_operator_budget_only` alike, so the ordering here is a
    // readability choice rather than a load-bearing one — unlike the merge/rescue ordering below,
    // which IS).
    //
    // EMPTY for every mount that declares no leg, and for every arm `declared_grid_source` does not
    // classify `InHand` — and an empty map is the identity: `RiskLimits::grid_for` returns the
    // scalars verbatim for every symbol and the field's `skip_serializing_if` keeps it out of the
    // serialized `RiskLimits`, so `vike_exec::engine_snapshot::state_hash` and every recorded
    // journal are unaffected.
    limits.grid_by_symbol = symbol_grids;
    // …and SAY so when a declared leg did not get one: it is then judged on the MOUNTED symbol's
    // tick, lot and floors — which is what every leg did before this wiring existed, so this is a
    // disclosure of a pre-existing degradation, not a new failure.
    symbol_grid::warn_ungridded_legs(venue, symbol, declared_legs, &limits.grid_by_symbol, &limits);
    // RunProfile wiring — closing the live gap: fold the operator's `[risk]` budget onto `limits`
    // HERE, once, after every arm above has already set the venue grid — so this ONE site covers
    // all ~12 venue arms instead of repeating the merge in each of them. See `merge_operator_budget`
    // for the merge itself (extracted so it is directly unit-testable — see that fn's doc and
    // `risk_profile_wiring.rs`'s regression test for why a test that never calls it pins nothing).
    //
    // LOAD-BEARING ORDERING: this merge runs BEFORE the `im_requirement` rescue below, never after.
    // `ProfileRisk::apply_to` takes EVERY operator-owned field (including `im_requirement`)
    // unconditionally from the profile — a profile that never mentions `im_requirement` carries
    // `None` for it, same as `ProfileRisk::default()`. Merging after the rescue would let that
    // `None` silently CLOBBER the conservative `Some(1.0)` default and disarm the buying-power gate
    // for every venue merely because SOME profile was supplied, even one that never touches
    // `im_requirement` — caught by this wiring's own test
    // (`profile_arms_the_limits_and_the_gate_denies_a_violating_order` failed against the
    // merge-after-rescue ordering before this comment was written). Merging first means the rescue
    // below still fires whenever NEITHER the venue fetch NOR the profile set `im_requirement`, and
    // a profile that DOES set it always wins (the rescue is a no-op on a `Some`).
    limits = merge_operator_budget(venue, limits, risk_profile);
    // Task 6 (armed-risk-defaults): arm the UNIVERSALLY-defaultable operator-budget fields —
    // see `arm_universal_defaults`'s own doc for the value-by-value justification (Nautilus/
    // im_requirement precedent) and why `required_free_bp_pct` needs no code here. SAME ordering
    // rule as the `im_requirement` rescue directly below (merge FIRST, rescue AFTER, never the
    // reverse): `merge_operator_budget` takes `max_orders_per_window`/`window_ms`
    // unconditionally from ANY profile threaded in — a profile that never mentions them carries
    // their serde-default `None`, same as `ProfileRisk::default()` — so rescuing BEFORE the merge
    // would let that `None` silently win the instant any profile is present, disarming them the
    // instant a profile that only sets, say, `max_notional_per_order` is supplied. That is the
    // exact `im_requirement` hazard this task's own brief calls out, reproduced for another
    // field had the order been wrong here too.
    limits = arm_universal_defaults(limits);
    // Phase B: enable the buying-power / margin gate with a conservative 1× default (im 1.0) —
    // `from_properties` overwrites `limits` and leaves `im_requirement` None, so set it here after
    // the venue grid is applied AND after the profile merge above (see that block's ordering
    // comment for why this must stay last). 1× means a flat account behaves as before (no
    // leverage); the leverage pill raises a symbol's leverage live via `Command::SetMargin`.
    //
    // This is now the SINGLE site that arms "no leverage unless asked" (issue #822 removed #817's
    // duplicate `max_leverage = 1.0` arming, which enforced nothing). An operator raises it with
    // `[risk] max_leverage`, which `ProfileRisk` converts into exactly this field — so a profile
    // that sets it lands here as a `Some` and the `.or` below is a no-op, unchanged.
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // ─── THE ACCOUNT-AGGREGATE EXPOSURE CEILING ───────────────────────────────────────────────
    //
    // The `policy.toml` ceiling reaching the pre-trade GATE. `vike_exec::RiskGate::check_inner`'s
    // `over-account-exposure` lane evaluates it against THIS account's whole projected book, and
    // this is the one site that arms it — `vike_config::Policy::max_account_exposure` carries the
    // argument for why the number lives in that file rather than in a run profile's `[risk]` table,
    // and `vike_exec::RiskLimits::max_account_exposure` is what it means once it arrives.
    //
    // `None` — no policy threaded in, or a deployment that wrote no line — leaves the field `None`
    // and the lane switched off, so every existing mount is byte-identical. It can only ever
    // REFUSE: nothing in that gate admits an order because this is set.
    //
    // ⚠ ORDERING. Folded AFTER `merge_operator_budget`, and belt-and-braces rather than
    // load-bearing: `ProfileRisk::apply_to` and `apply_operator_budget_only` both carry this field
    // through from `base` (it belongs to a THIRD owner — the policy file — like the price collar
    // beside it), so no profile can clobber it from either path. Folding here anyway keeps it with
    // the other post-merge arming, where a reader asking "what did the operator's files actually
    // arm" finds all of it at once.
    //
    // ⚠ It is PER ENGINE because this whole function is: one engine, one `RiskGate`, one copy of
    // the cap per `(venue, AccountLabel)`. A labelled second account of the same venue therefore
    // gets this budget measured over its OWN book — right while two labels really are two wallets,
    // and a DECLARED residual where they are not: `vike_config::venue_accounts`' shared-BOOK rule
    // (one venue ledger behind two labels) is REPORTED and mounts both engines, so that ledger can
    // then hold a MULTIPLE of the number the operator wrote. `make_engine_accounts`' warning names
    // this ceiling and its value when it is armed, so the multiplication is met at startup rather
    // than after a fill; `vike_exec::RiskLimits::max_account_exposure` and
    // `docs/decisions/0042-the-account-exposure-ceiling-is-a-policy-key.md` both carry it.
    //
    // ⚠ A NARROWING FOLD, never an assignment. `narrow_account_exposure` is a `min`, so the
    // no-raise property this ceiling claims is structural rather than resting on nobody else
    // writing the field — the same reason `vike_config::VenueMode::cap` is a `min`.
    limits.narrow_account_exposure(policy.and_then(|p| p.max_account_exposure));
    // ─── THE SIZING-EQUITY CEILING ────────────────────────────────────────────────────────────
    //
    // The `policy.toml` ceiling on the equity FIGURE this engine's sizing and admission lanes are
    // allowed to see. `vike_exec::ExecutionEngine::sizing_equity` is the one resolver that applies
    // it, and this is the one site that arms it.
    //
    // ⚠ WHY IT EXISTS, in one line: under `vike_exec::BalanceMode::Authoritative` resolved equity
    // is `venue wallet + unrealized`, the wallet is the venue's number for the WHOLE account the
    // credentials open, and every reconcile pass adopts it — so a third party funding or draining a
    // shared account moves what this daemon sizes and admits against. Disputing that figure was
    // built and abandoned;
    // `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md` carries
    // why, and this is the cap it chose instead (Hummingbot's `balance limit`/Freqtrade's
    // `available_capital` shape).
    //
    // ⚠ THE ASYMMETRY, stated where the value is armed rather than only in the record: a LOWER
    // equity figure is conservative for sizing and admission and DESTRUCTIVE for the margin-call
    // sweep, which liquidates on it. That is why the ceiling lands on a field only
    // `ExecutionEngine::sizing_equity` reads, and why `resolved_equity` — what
    // `vike_core`'s `sweep_margin_call_engine` and every report surface read — is deliberately
    // untouched by it. Arming this here cannot reach a liquidation decision.
    //
    // `None` — no policy threaded in, or a deployment that wrote no line — leaves the field `None`,
    // `sizing_equity` bit-identical to `resolved_equity`, and every existing mount byte-identical.
    //
    // ⚠ A NARROWING FOLD, never an assignment, for the reason the account ceiling above gives.
    limits.narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity));
    // Task 6's OTHER half (Freqtrade shape): `max_notional_per_order`/`max_total_exposure` have no
    // universal safe default, so a LIVE mount REFUSES TO START unless the operator supplied both —
    // see `require_live_risk_budget`'s doc. Since the PRE-CONNECT check at the top of this fn,
    // this post-merge site is the BACKSTOP only: on today's roster every live arm is covered by
    // `would_mount_live`, which already refused before the arm ran, so reaching here with a live
    // venue and a missing budget requires a FUTURE live arm added without a probe row — this
    // catches exactly that drift (after connect, as before #817's residual was closed). A
    // paper/backtest mount never reaches this branch (`live_venues` only ever gains an entry on a
    // genuinely live arm), so it is free to run with both caps unbounded, exactly as before.
    if live_venues.contains(&route_key) {
        require_live_risk_budget(venue, &limits, risk_profile.is_some())?;
    }
    // Effective per-venue fee schedule (fee model follow-up 1): prefer the live account-actual rate
    // the venue's `ReconClient` fetches (binance/bybit/okx/deribit producers) over the `static_default`
    // resolved once above, fail-soft. For a LIVE venue, fills already report the real per-fill
    // commission, so this resolved schedule is the SAME truth — now CONSUMED (not just logged): it is
    // tagged onto the engine so the snapshot's per-venue `fee_schedule` surfaces the real cost to the
    // GUI. A paper venue has `recon == None`, so `resolved == static_default` — the very schedule its
    // `PaperExecutionClient` above already fills with (paper-parity holds by construction).
    let fee_schedule = resolve_fee_schedule(venue, recon.as_deref(), static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // Per-symbol contract-multiplier grid for the mounted symbol (see `contract_size` above).
    // `None` when the venue reported nothing — an empty grid with the 1.0 scalar default, i.e.
    // EXACTLY the `None` this site always passed, so every non-deribit venue is byte-identical.
    let multipliers = multiplier_grid(symbol, contract_size);
    // Per-symbol ruling-margin-mode grid (see `default_margin_mode` above). `None` for `Cross` —
    // i.e. every venue but hyperliquid, and every hyperliquid asset that is not isolated-only —
    // which leaves the account byte-identical to one that never had this grid.
    let margin_modes = margin_mode_grid(symbol, default_margin_mode);
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(1.0, venue, multipliers, vike_exec::BalanceMode::Delta)
            .with_default_margin_modes(margin_modes),
        vike_exec::RiskGate::new(limits),
        client,
        venue,
        symbol,
    );
    engine.fee_schedule = Some(fee_schedule);
    // THE ROUTING IDENTITY. `ExecutionEngine::new` seeds `route_key` equal to `venue`, which is
    // exactly what `account_route_key` renders for `AccountLabel::Default` — so this assignment is
    // a no-op on every single-account mount and the only place in the workspace that ever makes the
    // two fields differ. `venue` itself is left alone deliberately: it is the key every per-venue
    // capability table is looked up under, and decorating it is the `"binance#2"` trap
    // `crates/vike-exec/tests/engine/route_key.rs` gates.
    engine.route_key = route_key;
    Ok((engine, recon))
}

/// The exact merge [`make_engine`] applies to fold an operator's `[risk]` budget onto a venue's
/// resolved `limits` — extracted to its own `pub(crate)` function so it is directly unit-testable
/// (see the tests below) rather than only reachable by driving a live `make_engine` call
/// end-to-end, which needs a REAL venue fetch to build a `GridSource::VenueFetched` base and so
/// cannot run from network-free CI (this is the exact gap a prior regression test claimed to
/// close but did not — see `risk_profile_wiring.rs`'s doc for that history).
///
/// `GridSource::VenueFetched` is hardcoded (never derived from a caller's profile `mode`): every
/// `limits` [`make_engine`] builds already came from a real venue fetch (or the permissive
/// post-fetch-failure fallback, which is likewise venue-owned, not operator-owned) — see
/// `ProfileRisk::apply_to`'s doc for why the venue's instrument-grid fields
/// (`tick_size`/`lot_size`/`min_qty`/`min_notional`) can never come from the profile here.
///
/// `profile: None` (no operator profile threaded in — every call site before this wiring existed,
/// and every call site today with no `[risk]` configured) leaves `limits` untouched:
/// BYTE-IDENTICAL to the mount before this parameter existed.
///
/// `profile: Some` normally merges via [`vike_exec::ProfileRisk::apply_to`]. If that profile
/// ALSO (illegally) sets a venue-owned instrument field, the merge does NOT fall back to leaving
/// the operator's budget entirely unarmed — that was the exact silent-degrade failure class a
/// review caught (a mode-mismatched profile dropping the ENTIRE operator budget on every venue
/// behind one log line): instead this falls back to
/// [`vike_exec::ProfileRisk::apply_operator_budget_only`], which still arms every operator-owned
/// field (`max_notional_per_order`/`max_total_exposure`/`max_orders_per_window`/`window_ms`/
/// `max_leverage`/`im_requirement`/`required_free_bp_pct`) and drops ONLY the venue fields the
/// profile should never have touched. Callers resolving a profile from a full `RunProfile` should
/// still prefer failing loud at resolution time via
/// `vike_core::RunProfile::risk_for_live_venue_mount` — this fallback is defense-in-depth for
/// any caller that reaches `make_engine` some other way, not a reason to skip that guard.
///
/// (Both mentions of that method are plain code spans, NOT intra-doc links, and must stay that
/// way: this crate does not depend on `vike-core` — deliberately, it sits BELOW the live core —
/// so rustdoc cannot resolve the path and a `[…]` link here is dead by construction.)
pub(crate) fn merge_operator_budget(
    venue: &str,
    limits: vike_exec::RiskLimits,
    profile: Option<&vike_exec::ProfileRisk>,
) -> vike_exec::RiskLimits {
    let Some(profile) = profile else { return limits };
    match profile.apply_to(limits.clone(), vike_exec::GridSource::VenueFetched) {
        Ok(merged) => merged,
        Err(e) => {
            tracing::error!(
                venue,
                error = %e,
                "risk profile rejected at merge (a venue-owned instrument field, or an \
                 out-of-range value — see `error`) — dropping ONLY the offending field(s); the \
                 operator's risk budget (max_notional_per_order/max_total_exposure/\
                 max_orders_per_window/window_ms/max_leverage + its derived im_requirement/\
                 required_free_bp_pct) still arms on this venue"
            );
            profile.apply_operator_budget_only(limits)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Task 6 (armed-risk-defaults, 2026-07-28): the five operator-budget fields split into two kinds —
// see the plan doc (`docs/superpowers/plans/2026-07-28-runprofile-wiring.md`, Task 6) and
// CLAUDE.md's `### Settings & configuration` for the full rationale. This is the split itself:
//
//   universally defaultable  -> `arm_universal_defaults` (Nautilus shape: armed at mount, always)
//   account-dependent        -> `require_live_risk_budget` (Freqtrade shape: refuse to start live)
//
// Neither NautilusTrader nor Freqtrade nor Hummingbot ships a live venue with an unbounded risk
// budget by default — see `make_engine`'s doc for the full competitor citation. Before this task,
// `RiskLimits::from_properties` armed only the venue's own instrument grid
// (`tick_size`/`lot_size`/`min_qty`/`min_notional`); every operator-budget field stayed `None`
// unless an operator remembered to supply a `[risk]` profile, and `RiskGate::check`'s `if let
// Some(cap) = …` guards mean `None` is not "unarmed", it is "the check never runs at all". A live
// run with no profile enforced the venue's lot grid and NOTHING else.
// ---------------------------------------------------------------------------------------------

/// Nautilus's `RiskEngineConfig` ships ARMED at `max_order_submit_rate: 100/00:00:01` — this is
/// that exact headline number, reconciled against every per-venue [`vike_bridge_core::ratelimit::RateGate`]
/// already wired in the bridge crates (net-hardening spec §A) so the two throttles never fight.
/// The tightest REAL venue order-rate gate today is Deribit's Tier4 matching-engine budget at
/// **5 per second** ([`vike_model::venue_rate_limits::DERIBIT`]`.orders`, wired by
/// `vike_deribit::ratelimit`); every other venue's own gate sits looser still (OKX 50/2s ≈ 25/s,
/// Bybit 18/s, Binance spot 90/10s ≈ 9/s and perp 270/10s = 27/s, Aster spot 90/min = 1.5/s and
/// perp 1080/min = 18/s — every one of them a row in that same table).
/// `RiskLimits::new()`'s `window_ms` is
/// already 1000 (1 second) and untouched by this rescue (see the field doc), so `100` here means
/// 100 orders/second — comfortably ABOVE every one of those real venue budgets.
///
/// That direction is load-bearing, not incidental: `RiskGate::check`'s throttle DENIES outright
/// (drops the order, no retry, no wait) the instant it trips, while a venue's own `RateGate` only
/// BLOCKS the wire send until a slot frees. Were this cap set below (or even near) a venue's real
/// budget, `RiskGate` would rate-deny legitimate sustained order flow the venue's own transport
/// would have simply queued and sent a moment later — silently dropping orders a slower path would
/// have delivered. Set safely above every real venue budget instead, this cap is inert in normal
/// operation (the venue's own blocking gate is always the first thing a real order stream meets)
/// and only trips a runaway loop submitting faster than ANY venue could ever legitimately sustain —
/// exactly the coarse circuit-breaker Nautilus's own default is.
const ARMED_MAX_ORDERS_PER_WINDOW: usize = 100;

// REMOVED (issue #822): `ARMED_MAX_LEVERAGE = 1.0`, armed here by #817. It set a field
// `RiskGate::check` never evaluates and nothing in the workspace clamps against, at exactly the
// same 1.0 the `im_requirement` rescue one line below `arm_universal_defaults`'s call site already
// enforces for real — an inert duplicate of an already-armed knob, which is what made the two
// names worth collapsing in the first place. "No leverage unless the operator asks" is still armed
// at that rescue, and is now ALSO what an operator's `[risk] max_leverage` reaches (it converts to
// `im_requirement` at the config edge — see `vike_exec::ProfileRisk`). The `max_leverage` field
// itself survives on `RiskLimits` as a frozen record of the DECLARED cap; see its own doc for why
// deleting it is not free.

/// Which credential tier a mainnet-capable CEX mount uses — the pure decision core of the
/// top-of-[`make_engine`] selection, factored out so the flag×creds matrix is unit-testable with no
/// process env and no network. Inputs are only the resolved mainnet flag and whether the LIVE tier
/// is PRESENT (the credential values themselves never affect the decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CexCredChoice {
    /// Flag UNSET ⇒ the DEMO tier, exactly as before this switch existed. A `None` demo tier then
    /// stays PAPER via the absent-credentials-is-the-live-gate rule, byte-identical to today.
    Demo,
    /// Flag SET and LIVE creds PRESENT ⇒ a real-money mainnet mount.
    LiveMainnet,
    /// Flag SET but NO live creds ⇒ PAPER. The safety-critical arm: it must NEVER fall back to demo
    /// creds on a mainnet host — absent live creds simply keep the venue paper (the live gate).
    MainnetNoCreds,
}

// Reached only (via `super::`) from this file's `#[cfg(test)]` modules — an unconditional import
// would go unused, and therefore warn, in a non-test build. Same shape as the
// `venue_arming_migration_message` import below. Kept HERE, below every documented constant in
// this file (`ARMED_MAX_ORDERS_PER_WINDOW` above), rather than at the top where it used to sit:
// `crates/vike-ops/tests/docs_constants_gate.rs`'s `code_only` stops reading a source file at its
// first INLINE `#[cfg(test)]` item (a `#[cfg(test)] use ...;` counts), so one of these placed near
// the top of the file hid `ARMED_MAX_ORDERS_PER_WINDOW` from that gate — measured red on the CI box
// (`every_claimed_default_still_has_that_value_in_the_code`) once two such imports existed above
// it. See `armed_policy`'s own doc in `arming.rs` for the fuller account of the same hazard.
#[cfg(test)]
use arming::{all_armed_policy, armed_policy, binance_withdraw_verdict, venue_arming_under};
// Only reached (via `super::`) from `preconnect_tests`, a `#[cfg(test)]` module — an unconditional
// import here would go unused, and therefore warn, in a non-test build.
#[cfg(test)]
use paper_fallback::venue_arming_migration_message;

#[cfg(test)]
mod paper_mount_halt_tests {
    /// Every paper fallback in `make_engine` arms the operator HALT sentinel.
    ///
    /// A mounted paper book is a MOUNT — it is what a venue with no credentials falls back to under
    /// the live gate — so `touch $VIKE_HALT_FILE` has to reach it. Before `paper_client` existed the
    /// eleven fallback arms each spelled the constructor verbatim and none of them armed anything,
    /// so a daemon whose venues were all on paper observed no HALT file at all, silently.
    ///
    /// ⚠ This asserts on the CONSTRUCTED book rather than on the source text, deliberately: a text
    /// gate over call sites cannot tell an armed construction from an unarmed one. Drop the
    /// `.with_halt_path(..)` from `paper_client` and this goes red.
    #[test]
    fn the_paper_fallback_every_venue_arm_uses_is_halt_armed() {
        let client = super::paper_client("binance", "BTCUSDT", vike_model::FeeSchedule::Free);
        assert!(
            client.halt_path().is_some(),
            "make_engine's paper fallback must arm the HALT sentinel — an operator rehearses the \
             kill switch on exactly this mount"
        );
    }
}

#[cfg(test)]
mod fee_schedule_tests {
    use super::resolve_fee_schedule;
    use vike_exec::recon::ReconClient;
    // Used only by the `fxcm`-gated live-intent assertions below.
    #[cfg(feature = "fxcm")]
    use vike_model::account_keys::AccountLabel;
    use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

    /// A `ReconClient` whose `fetch_fee_rates` returns a configurable outcome (the other report
    /// methods are irrelevant here). Mirrors the `FailingClient` test-double pattern in
    /// `vike_exec::recon::client`.
    struct FeeClient(Result<Option<FeeSchedule>, String>);
    impl ReconClient for FeeClient {
        fn fetch_order_status_reports(&self, _s: i64) -> Result<Vec<OrderStatusReport>, String> {
            Ok(vec![])
        }
        fn fetch_fill_reports(&self, _s: i64) -> Result<Vec<FillReport>, String> {
            Ok(vec![])
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            Ok(vec![])
        }
        fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
            self.0.clone()
        }
    }

    #[test]
    fn no_recon_falls_back_to_static_default() {
        let default = vike_model::fee_schedule_for("binance");
        assert_eq!(resolve_fee_schedule("binance", None, default), default);
    }

    #[test]
    fn live_some_is_preferred_over_static() {
        let live = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 2.0 };
        let rc = FeeClient(Ok(Some(live)));
        assert_eq!(
            resolve_fee_schedule("binance", Some(&rc), vike_model::fee_schedule_for("binance")),
            live
        );
    }

    #[test]
    fn none_and_error_both_fall_back_to_static() {
        let none = FeeClient(Ok(None));
        let err = FeeClient(Err("boom".to_string()));
        let expect = vike_model::fee_schedule_for("okx");
        assert_eq!(resolve_fee_schedule("okx", Some(&none), expect), expect);
        assert_eq!(resolve_fee_schedule("okx", Some(&err), expect), expect);
    }

    /// The permissive default a paper (or failed-pre-fetch) mount MUST carry: `from_properties` was
    /// NOT applied, so every grid field stays `0.0` (= unconstrained). `im_requirement` is the one
    /// field `make_engine` always sets (`Some(1.0)`, the conservative 1× buying-power default), so it
    /// is deliberately not asserted here. This is the "falls back to permissive on absent
    /// properties/creds" half of the RiskGate-property-grid contract — asserted for EVERY roster venue
    /// by `all_roster_venues_absent_creds_stay_paper_and_inert`, and for the feature-on connect-failure
    /// demotion by `ibkr_present_config_without_gateway_demotes_to_paper`. `venue` is threaded in so a
    /// failure names which venue's grid came back non-permissive.
    fn assert_permissive_grid(venue: &str, limits: &vike_exec::RiskLimits) {
        assert_eq!(limits.tick_size, None, "{venue}: permissive grid has no tick constraint");
        assert_eq!(limits.lot_size, None, "{venue}: permissive grid has no lot constraint");
        assert_eq!(limits.min_qty, None, "{venue}: permissive grid has no min-qty constraint");
        assert_eq!(
            limits.min_notional, None,
            "{venue}: permissive grid has no notional constraint"
        );
        // …and no PER-SYMBOL grid either. Asserted here so the whole roster carries it (this helper
        // is called from the roster-parameterized inert-default test): a mount that declares no leg
        // must leave `grid_by_symbol` empty, which is what keeps `RiskLimits::grid_for` returning
        // the scalars verbatim and keeps the serialized limits — hence
        // `vike_exec::engine_snapshot::state_hash` — byte-identical to before that map had a
        // producer. See `symbol_grid`'s module doc.
        assert!(
            limits.grid_by_symbol.is_empty(),
            "{venue}: a mount with no declared legs must carry no per-symbol grid"
        );
    }

    /// The three grid shapes the ctrader/alpaca/ibkr arms resolve, each run through the SAME
    /// `RiskLimits::from_properties` call the arms make, tightens the gate to non-default limits —
    /// the "sets non-default limits when properties are supplied" half of the contract, proven with
    /// synthetic grids (the live fetch itself needs a running venue and is exercised by each bridge's
    /// own parser tests: ctrader `risk_properties`, alpaca `parse_asset_properties`, ibkr
    /// `contract_details_to_properties`).
    #[test]
    fn resolved_grids_yield_non_default_limits() {
        let default = vike_exec::RiskLimits::new();
        // ctrader `risk_properties`: 10^-digits tick + centi-unit volume grid (EURUSD demo values).
        let ctrader = vike_model::SymbolProperties {
            tick_size: 1.0 / 100_000.0,
            step_size: 1000.0,
            min_qty: 1000.0,
            ..Default::default()
        };
        let l = vike_exec::RiskLimits::from_properties(&ctrader);
        assert_eq!(l.tick_size, Some(1.0 / 100_000.0));
        assert_eq!(l.lot_size, Some(1000.0), "step_size → lot_size");
        assert_eq!(l.min_qty, Some(1000.0));
        assert_ne!(l.tick_size, default.tick_size, "tighter than the permissive default (None)");

        // alpaca `/v1/assets` equity default: penny tick, whole-share step.
        let alpaca =
            vike_model::SymbolProperties { tick_size: 0.01, step_size: 1.0, ..Default::default() };
        let l = vike_exec::RiskLimits::from_properties(&alpaca);
        assert_eq!(l.tick_size, Some(0.01));
        assert_eq!(l.lot_size, Some(1.0));

        // ibkr `contractDetails`: min_tick / size_increment / min_size.
        let ibkr = vike_model::SymbolProperties {
            tick_size: 0.01,
            step_size: 1.0,
            min_qty: 1.0,
            ..Default::default()
        };
        let l = vike_exec::RiskLimits::from_properties(&ibkr);
        assert_eq!(l.tick_size, Some(0.01));
        assert_eq!(l.lot_size, Some(1.0));
        assert_eq!(l.min_qty, Some(1.0));
        assert_ne!(l.min_qty, default.min_qty, "tighter than the permissive default (None)");
    }

    /// ROSTER-PARAMETERIZED inert-default contract — the successor to the six hand-written
    /// near-duplicate `*_absent_creds_stays_paper_and_inert` twins (binance/ctrader/alpaca/ig/oanda/
    /// ibkr). For EVERY venue in the canonical `vike_model::VENUES` roster, mounting with an EMPTY
    /// `.env` map — no `{VENUE}_DEMO_*` / agent-wallet / private-key / OAuth creds anywhere — must
    /// yield the byte-identical PAPER engine: NO `ReconClient` handle (a paper venue never
    /// reconciles); NOT marked live (`live_venues` stays empty); tagged with the venue's static
    /// published fee schedule (a paper venue has no recon, so `resolve_fee_schedule` returns
    /// `static_default` == `fee_schedule_for(venue)` — the invariant the retired
    /// `make_engine_tags_paper_engine_with_resolved_schedule` pinned for binance, now generalized to
    /// the whole roster); and a PERMISSIVE RiskGate grid (`from_properties` never ran → every
    /// constraint field `None`).
    ///
    /// It also touches NO network — each live arm's cred/config gate returns before any connect: the
    /// crypto `(venue, Some)` arms don't match an absent cred (→ `_` paper); aster/hyperliquid/
    /// ctrader/alpaca/ig/oanda/ibkr self-gate on their own absent config (→ paper before any connect,
    /// e.g. hyperliquid's `config::load(..)?` and aster's `load_aster_credentials` short-circuit on
    /// the empty map); fxcm/dukascopy/polymarket have no arm at all (→ `_` paper).
    ///
    /// `recon_enabled` is deliberately `true` here, NOT `false`: this test's `recon.is_none()`
    /// assertion is about ABSENT CREDENTIALS being the live gate, and passing `false` would satisfy
    /// it through the new global reconcile gate instead, making it vacuous. With the gate on and an
    /// empty `.env`, every arm's cred/config check still returns first, so no inline recon factory is
    /// reached and the test stays offline.
    ///
    /// Iterating the roster is the whole point: a newly-added bridge crate (its id landing in
    /// `VENUES`) is AUTOMATICALLY held to this contract with ZERO new test code — the copy-drift
    /// surface the six hand-written twins were is gone (the `venues.rs`/`fees.rs` completeness-gate
    /// idiom, applied to the mount contract). aster is NOT special-cased despite being the one venue
    /// whose live tier arms on credential PRESENCE alone: with empty vars it resolves no
    /// agent-wallet creds and stays paper, so the loop is uniform.
    /// IBKR is covered in BOTH build modes — default (no `("ibkr", _)` arm → `_` paper) and
    /// `--features ibkr` (arm present but `load_ibkr_config_from` returns `None` before any connect) —
    /// because this test compiles unconditionally and the assertions hold either way.
    #[test]
    fn all_roster_venues_absent_creds_stay_paper_and_inert() {
        // The mounted symbol is IRRELEVANT on the paper path — no arm parses it before falling back
        // to paper, and `PaperExecutionClient` only stores it — so ONE representative symbol covers
        // every venue (each venue's own symbol format is exercised by its bridge's parser tests).
        const SYMBOL: &str = "BTCUSDT";
        let (tx, _rx) = vike_exec::event_channel(16);
        let vars = std::collections::HashMap::new(); // empty .env ⇒ absent creds for every venue
        // ⚠ The ARMING CEILING is opened for the WHOLE roster here, deliberately: this test's
        // subject is the OTHER gate — absent credentials — and a default (all-`paper`) policy would
        // satisfy every assertion below at the ceiling's early return, without any arm's own
        // cred/config check ever running. Arming everything is what keeps this the
        // absent-credentials contract rather than a second test of the ceiling.
        let armed = super::all_armed_policy(vike_config::VenueMode::Live);
        for &venue in vike_model::VENUES {
            let mut live = std::collections::HashSet::new();
            let (engine, recon) = super::make_engine(
                venue,
                SYMBOL,
                &vars,
                &tx,
                &mut live,
                true,
                None,
                None,
                None,
                Some(&armed),
            )
            .unwrap_or_else(|e| panic!("{venue}: paper mount must never refuse to start: {e}"));
            assert!(recon.is_none(), "{venue}: absent creds → paper, no reconcile handle");
            assert!(live.is_empty(), "{venue}: absent creds → venue not marked live");
            assert_eq!(
                engine.fee_schedule,
                Some(vike_model::fee_schedule_for(venue)),
                "{venue}: paper mount tagged with the venue's static fee schedule"
            );
            assert_permissive_grid(venue, &engine.gate.limits);
        }
    }

    /// THE WIRING GATE for the fee LANE: `make_engine` must key the fee table off the LANE the
    /// symbol routes to, not off the bare venue string.
    ///
    /// The table's lane rows are worth nothing unless this call site passes them, and this is the
    /// site that fills every `paper_client` fallback arm *and* supplies
    /// `resolve_fee_schedule`'s fallback — so mounting `BTCUSDT.P` on binance used to charge the
    /// SPOT 10/10 on a lane that costs 2/5 (5x the maker fee). Both symbol forms are asserted, so a
    /// revert to `fee_schedule_for(venue)` fails on the `.P` case while an over-eager lane that ate
    /// bare symbols fails on the other. The value pins live in `vike_model::fees`
    /// (`lane_rows_are_pinned`); this only proves the lane REACHES the engine.
    #[test]
    fn make_engine_keys_the_fee_schedule_off_the_symbol_lane() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let vars = std::collections::HashMap::new(); // empty .env ⇒ paper everywhere, no network
        let mount = |venue: &str, symbol: &str| {
            let mut live = std::collections::HashSet::new();
            // The trailing `None`s: no recon client, no properties recorder, no operator
            // risk_profile, and no policy.toml (the 10th param arrived with Phase 6c, #1057). This
            // test is about which fee ROW the lane key selects, so every other input stays at its
            // absent default.
            super::make_engine(venue, symbol, &vars, &tx, &mut live, true, None, None, None, None)
                .unwrap_or_else(|e| panic!("{venue}/{symbol}: paper mount must start: {e}"))
                .0
                .fee_schedule
                .expect("make_engine always tags a schedule")
        };
        for venue in ["binance", "aster"] {
            assert_eq!(
                mount(venue, "BTCUSDT.P"),
                vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT.P")),
                "{venue}: a `.P` mount must be tagged with its PERP lane's schedule"
            );
            assert_eq!(
                mount(venue, "BTCUSDT"),
                vike_model::fee_schedule_for(venue),
                "{venue}: a bare mount must stay byte-identical to the pre-lane behavior"
            );
        }
        // binance is the venue whose lanes are actually priced apart — assert the engine really
        // ends up with two DIFFERENT schedules, so a lane resolution that silently collapsed
        // (`fee_lane` returning the bare id, a reverted call site) cannot pass this test.
        assert_ne!(
            mount("binance", "BTCUSDT.P"),
            mount("binance", "BTCUSDT"),
            "binance perp and spot mounts must not share a fee schedule"
        );
        // A single-exec-lane venue is unaffected by the suffix (bybit's exec is linear-perp only).
        assert_eq!(mount("bybit", "BTCUSDT.P"), mount("bybit", "BTCUSDT"));
    }

    /// Feature-on coverage of the actual `("ibkr", _)` arm body: config PRESENT but pointed at dead
    /// ports (no TWS socket, no CP Gateway), so `IbkrExecutionClient::connect` fails fast
    /// (connection refused) and the arm DEMOTES to PAPER — and the cpapi recon client resolves
    /// `None` for the same reason. Needs NO running Gateway. SELF-SKIPS if a Gateway happens to be
    /// reachable on the chosen ports (then IBKR would legitimately go live), so the assertion never
    /// fires falsely on a dev box that has TWS/Gateway running.
    #[cfg(feature = "ibkr")]
    #[test]
    fn ibkr_present_config_without_gateway_demotes_to_paper() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        // Dead ports: nothing listens on 127.0.0.1:6553{4,3}, so both the socket exec connect and the
        // cpapi recon tickle get an immediate connection-refused.
        let vars: std::collections::HashMap<String, String> = [
            ("IBKR_DEMO_ACCOUNT", "DUTEST000"),
            ("IBKR_DEMO_BACKEND", "socket"),
            ("IBKR_DEMO_PORT", "65534"),
            ("IBKR_DEMO_CPAPI_URL", "https://127.0.0.1:65533"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        // ⚠ A RISK BUDGET IS PART OF THIS FIXTURE, and the fxcm twin below deliberately has none.
        // The asymmetry is BY CONSTRUCTION — do not "harmonize" the two by deleting this.
        //
        // `make_engine_for_account` runs a PRE-CONNECT refusal (the #817 "the refusal happens
        // POST-connect" fix, Freqtrade's shape): when `account_arming_under` says this mount is
        // live-INTENT, a missing `max_notional_per_order`/`max_total_exposure` is
        // `MountError::MissingRiskBudget` BEFORE any arm dials anything. That probe is INTENT-based
        // and names this venue in its own comment: present live config is the operator's declared
        // intent, even where the arm would later demote to paper. ibkr's `arming.rs` row reads
        // `load_ibkr_config_for_account(Demo, …)`, which the vars above satisfy, and the ceiling
        // below arms it — so ibkr IS live-intent here, and with no budget the mount refuses without
        // ever reaching the connect-failure demotion this test exists to observe. (Measured: on
        // this test's first ever execution, once the `ibkr` CI lane widened to compile it, that is
        // exactly how it failed.)
        //
        // fxcm's row answers `(Paper, SdkAbsent)` on its FIRST conjunct — `sdk_linked()` is false
        // on every CI runner — so its twin is not live-intent and never reaches the budget gate.
        // ibkr has no such conjunct available: its Gateway's liveness is knowable ONLY by
        // attempting a connect, which is precisely the I/O this gate exists to precede.
        //
        // The two caps below are the exact pair `require_live_risk_budget` reads, and nothing else
        // is set — a profile carrying no venue-owned field arms the operator budget and leaves the
        // venue GRID alone, so `assert_permissive_grid` below is unaffected and still asserts what
        // it always did.
        let budget = vike_exec::ProfileRisk {
            max_notional_per_order: Some(100.0),
            max_total_exposure: Some(500.0),
            ..Default::default()
        };
        // The `live` set is checked BEFORE the `Result` is unwrapped: on the rare dev box where a
        // Gateway IS unexpectedly reachable this venue legitimately goes live, and every assertion
        // below would then be false for a correct reason — so that box must self-skip, exactly like
        // the "Gateway reachable" branch this comment guards. With the budget supplied the mount
        // returns `Ok` on BOTH branches, so the skip is the only thing separating them.
        // `recon_enabled: true` on purpose — the "no Gateway → recon unwired" assertion below is
        // about the cpapi handshake FAILING on a dead port, so the factory must actually be
        // reached; `false` would satisfy it through the global gate and pin nothing.
        let result = super::make_engine(
            "ibkr",
            "AAPL.SMART.USD",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            Some(&budget),
            // ⚠ The ARMING CEILING must permit ibkr, or the arm below is never reached at all and
            // this test passes against the paper early return — every assertion here is also true
            // of a capped mount, so without this line it would be vacuous rather than red.
            Some(&super::armed_policy("ibkr", vike_config::VenueMode::Demo)),
        );
        if !live.is_empty() {
            eprintln!("skipping: an IBKR Gateway is unexpectedly reachable on the test ports");
            return;
        }
        let (engine, recon) = result.expect(
            "budget supplied + no reachable Gateway -> the mount demotes to paper, not Err",
        );
        assert!(recon.is_none(), "no Gateway → recon unwired");
        assert!(live.is_empty(), "connect failure → not marked live (demoted to paper)");
        assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("ibkr")));
        // Demoted to paper: the `contractDetails` pre-fetch is never reached (it lives inside the
        // `Ok(client)` branch), so limits stay permissive — same as every paper mount.
        assert_permissive_grid("ibkr", &engine.gate.limits);
    }

    /// Feature-on coverage of the `("fxcm", _)` arm body, and the ONLY branch of it any CI runner
    /// can reach: credentials PRESENT, ForexConnect NOT linked ⇒ the arm REFUSES the live mount and
    /// lands on paper.
    ///
    /// This is the branch that matters most, because the behaviour it rules out is silent. A stub
    /// build's `FxcmSession::login` returns `Unavailable`, the exec thread returns immediately, and
    /// `FxcmExecutionClient::spawn` — which is infallible — hands back a client that accepts every
    /// submit and forwards it to a thread that is not there. Without this refusal an operator with
    /// FXCM credentials and an SDK-less binary would see the venue reported LIVE, place orders, and
    /// get neither fills nor rejections: the no-silent-vanish contract failing at the mount.
    ///
    /// Needs no SDK, no credentials of any real account and no network: the refusal happens before
    /// `spawn`, so nothing dials FXCM. SELF-SKIPS on a box that HAS the SDK linked, where mounting
    /// live is the correct outcome and these asserts would legitimately be false.
    #[cfg(feature = "fxcm")]
    #[test]
    fn fxcm_credentials_without_a_linked_sdk_refuse_the_live_mount() {
        if vike_fxcm::sdk_linked() {
            eprintln!("skipping: this binary HAS ForexConnect linked, so fxcm mounts live here");
            return;
        }
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let vars: std::collections::HashMap<String, String> =
            [("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "not-a-real-password")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        // `recon_enabled: true` deliberately — the "no SDK ⇒ recon unwired" assertion is about the
        // refusal happening BEFORE the recon factory is reached, so the gate must be open; `false`
        // would satisfy it through the global gate and pin nothing.
        // ⚠ The ARMING CEILING permits fxcm here, so the arm is genuinely reached and it is the
        // SDK refusal being pinned. A capped mount would satisfy every assertion below without the
        // arm ever running.
        let armed = super::armed_policy("fxcm", vike_config::VenueMode::Demo);
        let (engine, recon) = super::make_engine(
            "fxcm",
            "EURUSD",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&armed),
        )
        .expect("a paper mount must never refuse to start");
        assert!(
            live.is_empty(),
            "a stub build must NOT mark fxcm live — its exec client would discard every order"
        );
        assert!(recon.is_none(), "the refusal precedes the recon factory, so nothing is wired");
        assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("fxcm")));
        assert_permissive_grid("fxcm", &engine.gate.limits);

        // The CONTROL: the same binary with the credentials REMOVED reaches the same paper outcome
        // by the ordinary unconfigured path, so the assertions above pin the SDK refusal and not
        // some unrelated fxcm breakage that would make every fxcm mount paper.
        let mut live2 = std::collections::HashSet::new();
        let (_e, recon2) = super::make_engine(
            "fxcm",
            "EURUSD",
            &std::collections::HashMap::new(),
            &tx,
            &mut live2,
            true,
            None,
            None,
            None,
            Some(&armed),
        )
        .expect("a paper mount must never refuse to start");
        assert!(live2.is_empty() && recon2.is_none());
        // …and the PURE probe is what separates the two causes: with a linked SDK these same
        // credentials WOULD be live intent, while the empty map would not.
        assert!(super::fxcm_live_intent(true, &AccountLabel::Default, &vars));
        assert!(!super::fxcm_live_intent(
            true,
            &AccountLabel::Default,
            &std::collections::HashMap::new()
        ));
    }

    /// A syntactically valid secp256k1 key that is NOT a real account — the arm's gates return
    /// before anything is ever signed with it, which is the property these tests assert.
    #[cfg(feature = "polymarket")]
    const POLY_TEST_KEY: &str =
        "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

    /// A polymarket test can only assert "no network call" if neither gate is exported in the REAL
    /// process env (both gates read it as well as the map). Skip rather than produce a false green.
    #[cfg(feature = "polymarket")]
    fn poly_gates_clean(vars: &std::collections::HashMap<String, String>) -> bool {
        if vike_polymarket::poly_reconcile_enabled(vars) || vike_polymarket::poly_exec_enabled(vars)
        {
            eprintln!("skipping: POLY_RECONCILE / POLY_EXEC is exported in this process env");
            return false;
        }
        true
    }

    /// Every polymarket test below wants the `("polymarket", _)` ARM to actually run, so each one
    /// arms the ceiling to `live` — the tier that venue's exec requires, since it has no testnet.
    /// Without it the mount returns at the paper early return and the venue's own double gate,
    /// which is what these tests exist to pin, is never consulted.
    #[cfg(feature = "polymarket")]
    fn poly_armed() -> super::MountPolicy {
        super::armed_policy("polymarket", vike_config::VenueMode::Live)
    }

    /// Feature-on coverage of the `("polymarket", _)` arm body, the OFFLINE half: BOTH gates are the
    /// FIRST things read, so creds present + both unset ⇒ no reconcile handle, no exec client AND no
    /// network call (the L2 `/auth/derive-api-key` round-trip lives behind them). This is the
    /// byte-identical-to-a-default-build case the double gate exists to guarantee.
    ///
    /// ⚠ **Since S2 this is also the SECOND gate's assertion, and the wording matters because the
    /// first draft of it was wrong.** `vike_ops::reconcile_config::reconcile_gate` turns the master
    /// gate ON for every mount that arms a live venue account. That default DOES reach Polymarket —
    /// it is the same driver, mounted over the same `recon_clients` vector — so it is NOT true that
    /// this venue "did not change": what changed is that its OUTER gate is now supplied by the box
    /// rather than typed by a person, leaving `POLY_RECONCILE=1` as the one remaining act. It is
    /// still one act more than any other venue needs, and it is still what this test pins: the
    /// `recon_enabled: true` below passes the master gate in its DEFAULT-ON state and the test
    /// demands `recon.is_none()` anyway, so a PR that dropped the venue gate as redundant reddens
    /// here. Why the venue gate must survive the default:
    /// `crates/bridges/polymarket/CLAUDE.md` (reconciling a live Polymarket account against a paper
    /// engine, and the venue's absence of any testnet), and
    /// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` for what the owner is
    /// being asked to ratify.
    #[cfg(feature = "polymarket")]
    #[test]
    fn polymarket_without_the_gates_is_inert_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let vars: std::collections::HashMap<String, String> =
            [("POLY_PRIVATE_KEY".to_string(), POLY_TEST_KEY.to_string())].into_iter().collect();
        if !poly_gates_clean(&vars) {
            return;
        }
        // `recon_enabled: true` on purpose: this arm reads BOTH gates
        // (`poly_recon_wanted`), and passing the master one in its default-on state is what makes
        // the assertion below about the VENUE gate rather than about a mount that was off anyway.
        let (engine, recon) = super::make_engine(
            "polymarket",
            "71321045679252212594626385532706912750332728571942532289631379312455583992563",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&poly_armed()),
        )
        .expect("stays paper (no exec gate) -> the refusal check must not fire");
        assert!(
            recon.is_none(),
            "POLY_RECONCILE unset → no reconcile handle (and no network call)"
        );
        assert!(live.is_empty(), "POLY_EXEC unset → exec stays PAPER, never marked live");
        assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("polymarket")));
        assert_permissive_grid("polymarket", &engine.gate.limits);
    }

    /// Recon gate ON but creds ABSENT ⇒ still no handle, still no network call (the factory's own
    /// absent-credentials-is-the-live-gate return precedes its `ensure_l2` round-trip). Proves the
    /// two gates compose in both orders, offline.
    #[cfg(feature = "polymarket")]
    #[test]
    fn polymarket_with_the_recon_gate_but_no_creds_stays_inert() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let vars: std::collections::HashMap<String, String> =
            [(vike_polymarket::POLY_RECONCILE_ENV.to_string(), "1".to_string())]
                .into_iter()
                .collect();
        let (_engine, recon) = super::make_engine(
            "polymarket",
            "0",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&poly_armed()),
        )
        .expect("stays paper (no exec gate) -> the refusal check must not fire");
        assert!(recon.is_none(), "gate on but no POLY_PRIVATE_KEY → reconcile-inert");
        assert!(live.is_empty());
    }

    /// The EXEC gate's twin: `POLY_EXEC=1` with NO `POLY_PRIVATE_KEY` ⇒ no live client, venue stays
    /// paper, and — the part that matters for CI — no network call, because
    /// `live_mount_from_vars` returns on absent credentials before its `ensure_l2` round-trip.
    #[cfg(feature = "polymarket")]
    #[test]
    fn polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let vars: std::collections::HashMap<String, String> =
            [(vike_polymarket::POLY_EXEC_ENV.to_string(), "1".to_string())].into_iter().collect();
        let (engine, recon) = super::make_engine(
            "polymarket",
            "0",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&poly_armed()),
        )
        .expect("no key -> stays paper -> the refusal check must not fire");
        assert!(live.is_empty(), "POLY_EXEC=1 but no key → paper, never marked live");
        assert!(recon.is_none());
        assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("polymarket")));
    }

    /// …and with an UNUSABLE key PRESENT: CONTRACT CHANGED by the pre-connect refusal (#817
    /// residual). Key material the operator wrote + the explicit `POLY_EXEC=1` flag IS declared
    /// live intent (`would_mount_live`'s polymarket row, same stance as hyperliquid's), so with
    /// no risk budget the mount now REFUSES — before the factory would even try (and fail) the
    /// EOA derivation — instead of silently falling back to paper as it did before. Still
    /// entirely offline: the refusal precedes any network. The no-key case above keeps the paper
    /// fallback (a flag with no key can never mount live, so it is not intent).
    #[cfg(feature = "polymarket")]
    #[test]
    fn polymarket_exec_gate_with_a_bad_key_refuses_preconnect_without_budget() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let vars: std::collections::HashMap<String, String> = [
            (vike_polymarket::POLY_EXEC_ENV.to_string(), "1".to_string()),
            ("POLY_PRIVATE_KEY".to_string(), "not-a-key".to_string()),
        ]
        .into_iter()
        .collect();
        let err = match super::make_engine(
            "polymarket",
            "0",
            &vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&poly_armed()),
        ) {
            Err(e) => e,
            Ok(_) => panic!("key present + POLY_EXEC=1 + no budget must refuse pre-connect"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("polymarket"), "must name the venue: {msg}");
        assert!(msg.contains("max_notional_per_order") && msg.contains("max_total_exposure"));
        assert!(live.is_empty(), "the refusal precedes the arm — nothing recorded live");

        // …and the ARMING CEILING is what decides whether any of that is reached at all. Under
        // `demo` the SAME configuration is not live intent: polymarket has no testnet, so `demo`
        // names a tier that does not exist and its exec arm refuses rather than arming the only
        // tier there is (REAL money on Polygon mainnet). Paper, offline, and no refusal to start.
        let mut demoted = std::collections::HashSet::new();
        let (_engine, recon) = super::make_engine(
            "polymarket",
            "0",
            &vars,
            &tx,
            &mut demoted,
            true,
            None,
            None,
            None,
            Some(&super::armed_policy("polymarket", vike_config::VenueMode::Demo)),
        )
        .expect("a demo-capped polymarket is PAPER, and paper never refuses to start");
        assert!(demoted.is_empty(), "a `demo` ceiling must not arm a mainnet-only venue");
        assert!(recon.is_none(), "POLY_RECONCILE is unset here, so nothing reconciles either");
    }

    /// The go-live credential-selection matrix (completes #771). Pure — no process env, no network.
    /// The four combinations of the `{VENUE}_MAINNET` flag × LIVE-cred presence, each pinned to the
    /// tier the mount uses. The two safety-critical rows are the bottom two: flag set with live creds
    /// is the ONLY path to real money, and flag set WITHOUT live creds degrades to PAPER — never
    /// demo-on-mainnet.
    #[test]
    fn cex_cred_choice_matrix() {
        use super::CexCredChoice::*;
        // Flag UNSET ⇒ DEMO regardless of whether live creds happen to exist (byte-identical to
        // before this switch existed — the unset path never even looks at the live tier).
        assert_eq!(super::cex_cred_choice(false, false), Demo);
        assert_eq!(super::cex_cred_choice(false, true), Demo);
        // Flag SET + LIVE creds present ⇒ the ONLY real-money path.
        assert_eq!(super::cex_cred_choice(true, true), LiveMainnet);
        // Flag SET + NO live creds ⇒ PAPER, never demo-on-mainnet (absent-creds-is-the-live-gate).
        assert_eq!(super::cex_cred_choice(true, false), MainnetNoCreds);
    }

    /// Only binance/bybit/okx carry a `{VENUE}_MAINNET` credential switch; every other roster venue
    /// resolves `false` WITHOUT reading any env — and, since STEP 2, without the `.env` map being
    /// able to arm it either: an arming entry for EVERY roster venue is supplied here and only the
    /// three CEX venues may see it. That is the unset-flag safety guarantee for the rest of the
    /// roster, now proven against the new `.env` source rather than merely against process env.
    /// (The three CEX venues are deliberately not asserted here, where a stray exported flag could
    /// flip the result; their grammar is exercised env-free by each bridge's own `mainnet_from`
    /// tests and by `vike_bridge_core::mainnet`'s.)
    #[test]
    fn only_cex_venues_have_a_mainnet_cred_switch() {
        let mut vars = std::collections::HashMap::new();
        for &v in vike_model::VENUES {
            vars.insert(format!("{}_MAINNET", v.to_uppercase()), "1".to_string());
        }
        for &v in vike_model::VENUES {
            if matches!(v, "binance" | "bybit" | "okx") {
                continue;
            }
            assert!(
                !super::cex_mainnet_enabled(v, &vars),
                "{v}: no mainnet cred switch → always the demo/default path, even with an arming \
                 `.env` entry present"
            );
        }
    }

    /// STEP 2 of the `{VENUE}_MAINNET` convergence, at the mount: a `{VENUE}_MAINNET=1` line in the
    /// workspace `.env` map now ARMS each CEX venue — it used to parse as UNSET (the `.env` is
    /// never exported to process env) and silently keep the mount on demo, which was the audit
    /// finding. Asserted only in the arming direction, the one a stray exported flag cannot spoof.
    #[test]
    fn a_dotenv_only_mainnet_line_arms_every_cex_venue() {
        for v in ["binance", "bybit", "okx"] {
            let mut vars = std::collections::HashMap::new();
            vars.insert(format!("{}_MAINNET", v.to_uppercase()), "1".to_string());
            assert!(
                super::cex_mainnet_enabled(v, &vars),
                "{v}: a `.env`-only `{}_MAINNET=1` must arm mainnet (STEP-2 gain)",
                v.to_uppercase()
            );
        }
    }

    // ---- api-key permissions, STEP-2 (the binance live arm's pre-arm gate) --------------------

    /// The pure verdict core: a KNOWN withdraw-capable key REFUSES (so the live arm's guard fails
    /// and binance falls through to paper), the operator override forces `Allow`, and a trade-only
    /// key arms.
    #[test]
    fn a_known_withdraw_capable_key_refuses_unless_overridden() {
        use vike_bridge_core::key_permissions::{KeyPermissions, WithdrawGate};
        let withdraw = KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN };
        assert_eq!(super::binance_withdraw_verdict(Ok(withdraw), false), WithdrawGate::Refuse);
        assert_eq!(super::binance_withdraw_verdict(Ok(withdraw), true), WithdrawGate::Allow);
        let trade_only = KeyPermissions {
            can_withdraw: Some(false),
            can_trade: Some(true),
            ip_restricted: Some(true),
        };
        assert_eq!(super::binance_withdraw_verdict(Ok(trade_only), false), WithdrawGate::Allow);
    }

    /// FAIL-OPEN on introspection: a fetch error (and an all-Unknown body) is NOT evidence of a
    /// withdraw-capable key, so it must never refuse — the same permissive shape as the
    /// `RiskLimits::from_properties` pre-fetch fallback next to the call site.
    #[test]
    fn an_unknown_or_failed_key_probe_never_refuses() {
        use vike_bridge_core::key_permissions::{KeyPermissions, WithdrawGate};
        let err = Err("venue error -2015: Invalid API-key".to_string());
        assert_eq!(super::binance_withdraw_verdict(err, false), WithdrawGate::Allow);
        assert_eq!(
            super::binance_withdraw_verdict(Ok(KeyPermissions::UNKNOWN), false),
            WithdrawGate::Allow
        );
    }

    /// The OFF/DEFAULT path: a DEMO mount short-circuits to `Allow` with NO network call — `sapi`
    /// is mainnet-only, so there is nothing to introspect on the testnet host. This is why a
    /// credential-free CI run (and every demo mount) is byte-identical to before this gate existed;
    /// the test would hang or fail on a sandboxed runner if the demo path probed anything.
    #[test]
    fn a_demo_mount_never_probes_key_permissions() {
        use vike_bridge_core::key_permissions::WithdrawGate;
        let creds = vike_bridge_core::Credentials {
            api_key: "test-key".to_string(),
            api_secret: "test-secret".to_string(),
            passphrase: None,
        };
        assert_eq!(super::binance_withdraw_gate(false, &creds), WithdrawGate::Allow);
    }

    /// A venue that reports no contract size yields NO grid — the literal `None` this site passed
    /// before the wiring existed, so `Account::multiplier_of` falls through to its 1.0 scalar and
    /// every non-deribit venue mounts byte-identically.
    #[test]
    fn absent_contract_size_yields_no_grid() {
        assert!(super::multiplier_grid("BTCUSDT", 0.0).is_none());
        // an explicit 1.0 is arithmetically the same as no grid — collapsed, not carried
        assert!(super::multiplier_grid("BTC-8JUL26-62000-C", 1.0).is_none());
    }

    /// A real contract size lands in the grid under the mounted symbol, which is exactly what
    /// `Account::multiplier_of` (and therefore `validate_with_multiplier` + the snapshot the GUI
    /// reads) looks up. This is the assertion that the root bug is closed.
    #[test]
    fn real_contract_size_lands_in_the_grid() {
        let grid = super::multiplier_grid("BTC-PERPETUAL", 10.0).expect("non-1.0 → a grid");
        assert_eq!(grid.get("BTC-PERPETUAL"), Some(&10.0));
        assert_eq!(grid.len(), 1, "only the mounted symbol");
    }

    /// A degenerate venue value must not produce a 0.0/negative multiplier grid — it folds to the
    /// absent case, since a 0.0 multiplier would make every order measure as zero notional.
    #[test]
    fn degenerate_contract_size_yields_no_grid() {
        for bad in [-5.0, f64::NAN, f64::INFINITY] {
            assert!(super::multiplier_grid("X", bad).is_none(), "{bad} must not build a grid");
        }
    }

    /// End-to-end through the type the engine actually consults: a grid built from a contract size
    /// makes `Account::multiplier_of` return it, while an unlisted symbol stays 1.0.
    #[test]
    fn grid_drives_account_multiplier_of() {
        let acct = vike_exec::Account::new(
            1.0,
            "deribit",
            super::multiplier_grid("BTC-PERPETUAL", 10.0),
            vike_exec::BalanceMode::Delta,
        );
        assert_eq!(acct.multiplier_of("BTC-PERPETUAL"), 10.0, "the mounted symbol's contract size");
        assert_eq!(acct.multiplier_of("ETH-PERPETUAL"), 1.0, "unlisted → the scalar default");

        // and the no-contract-size venue is byte-identical to the pre-wiring `None`
        let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
        let wired = vike_exec::Account::new(
            1.0,
            "binance",
            super::multiplier_grid("BTCUSDT", 0.0),
            vike_exec::BalanceMode::Delta,
        );
        assert_eq!(plain.multiplier_of("BTCUSDT"), wired.multiplier_of("BTCUSDT"));
    }

    /// [`super::margin_mode_grid`]'s collapse rule, the margin-axis twin of the three
    /// `multiplier_grid` tests above: `Cross` — the resolved mode on 13 of the 14 roster venues and
    /// on every ordinary hyperliquid asset — yields NO grid, so `Account` keeps the empty-map
    /// short-circuit and mounts byte-identically. A non-`Cross` mode lands under the mounted symbol,
    /// which is exactly the key `Account::default_margin_mode_of` looks up on open-from-flat.
    #[test]
    fn margin_mode_grid_collapses_cross_and_carries_the_rest() {
        use vike_model::MarginMode;
        assert!(super::margin_mode_grid("BTC", MarginMode::Cross).is_none(), "Cross ⇒ no grid");

        for mode in [MarginMode::Isolated, MarginMode::Cash] {
            let grid = super::margin_mode_grid("CASHCAT", mode).expect("non-Cross ⇒ a grid");
            assert_eq!(grid.get("CASHCAT"), Some(&mode));
            assert_eq!(grid.len(), 1, "only the mounted symbol");
        }
    }

    /// End-to-end through the type the fold actually consults, mirroring
    /// `grid_drives_account_multiplier_of` directly above: the grid drives
    /// `Account::default_margin_mode_of`, an unlisted symbol stays `Cross`, and the collapsed-`Cross`
    /// mount is indistinguishable from the `None` every other venue passes.
    #[test]
    fn margin_mode_grid_drives_account_default_margin_mode_of() {
        use vike_model::MarginMode;
        let acct = vike_exec::Account::new(1.0, "hyperliquid", None, vike_exec::BalanceMode::Delta)
            .with_default_margin_modes(super::margin_mode_grid("CASHCAT", MarginMode::Isolated));
        assert_eq!(acct.default_margin_mode_of("CASHCAT"), MarginMode::Isolated);
        assert_eq!(acct.default_margin_mode_of("BTC"), MarginMode::Cross, "unlisted → Cross");

        let plain = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta);
        let wired = vike_exec::Account::new(1.0, "binance", None, vike_exec::BalanceMode::Delta)
            .with_default_margin_modes(super::margin_mode_grid("BTCUSDT", MarginMode::Cross));
        assert_eq!(
            plain.default_margin_mode_of("BTCUSDT"),
            wired.default_margin_mode_of("BTCUSDT")
        );
    }

    // ---------------------------------------------------------------------------------------
    // merge_operator_budget — the ACTUAL merge site `make_engine` calls, pinned DIRECTLY (unlike
    // a test that only drives `ProfileRisk::apply_to` by hand and never touches this function —
    // see this fn's own doc for why that distinction matters). The base below is shaped like a
    // REAL `RiskLimits::from_properties` fetch (populated instrument grid), so flipping the
    // hardcoded `GridSource::VenueFetched` inside `merge_operator_budget` to `NoGridFetched` would
    // make `venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback` below observe
    // the profile's illegal `tick_size` silently WIN instead of being dropped — failing first.
    // ---------------------------------------------------------------------------------------

    fn venue_fetched_limits() -> vike_exec::RiskLimits {
        vike_exec::RiskLimits {
            tick_size: Some(0.5),
            lot_size: Some(0.01),
            min_qty: Some(0.01),
            min_notional: Some(10.0),
            ..vike_exec::RiskLimits::new()
        }
    }

    #[test]
    fn merge_operator_budget_none_leaves_limits_untouched() {
        let base = venue_fetched_limits();
        let got = super::merge_operator_budget("binance", base.clone(), None);
        assert_eq!(got, base, "no profile threaded in must be a byte-identical no-op");
    }

    /// THE point of pinning this at the `merge_operator_budget` call site rather than only at
    /// `ProfileRisk::apply_to`: a clean profile (no venue-owned fields) must both arm the operator
    /// budget AND leave the REAL fetched venue grid alone — proving this function actually routes
    /// through `VenueFetched`, not some other source.
    #[test]
    fn merge_operator_budget_arms_operator_fields_and_keeps_the_venue_grid() {
        let base = venue_fetched_limits();
        let profile = vike_exec::ProfileRisk {
            max_notional_per_order: Some(100.0),
            max_total_exposure: Some(500.0),
            ..vike_exec::ProfileRisk::default()
        };
        let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
        assert_eq!(got.tick_size, base.tick_size, "venue grid must stay the REAL fetched value");
        assert_eq!(got.lot_size, base.lot_size);
        assert_eq!(got.min_qty, base.min_qty);
        assert_eq!(got.min_notional, base.min_notional);
        assert_eq!(got.max_notional_per_order, Some(100.0));
        assert_eq!(got.max_total_exposure, Some(500.0));
    }

    /// BLOCKING-2(b) regression: a profile that ALSO (illegally) sets a venue-owned field over a
    /// REAL fetched grid must not zero the operator's whole budget — only the offending venue
    /// field is dropped (kept as the real fetched value), while every operator-owned field the
    /// profile set still arms.
    #[test]
    fn venue_owned_field_conflict_arms_the_operator_budget_via_the_fallback() {
        let base = venue_fetched_limits();
        let profile = vike_exec::ProfileRisk {
            tick_size: Some(999.0), // illegal under a real venue fetch
            max_notional_per_order: Some(100.0),
            max_total_exposure: Some(500.0),
            max_orders_per_window: Some(5),
            window_ms: 2000,
            ..vike_exec::ProfileRisk::default()
        };
        let got = super::merge_operator_budget("binance", base.clone(), Some(&profile));
        assert_eq!(
            got.tick_size, base.tick_size,
            "the illegal venue-owned override must be dropped, not honored"
        );
        assert_eq!(
            got.max_notional_per_order,
            Some(100.0),
            "the operator budget must still arm despite the unrelated venue-field conflict"
        );
        assert_eq!(got.max_total_exposure, Some(500.0));
        assert_eq!(got.max_orders_per_window, Some(5));
        assert_eq!(got.window_ms, 2000);
    }

    // ---------------------------------------------------------------------------------------
    // Task 6 (armed-risk-defaults) — `arm_universal_defaults` / `require_live_risk_budget`,
    // pinned DIRECTLY (the same reasoning as `merge_operator_budget`'s own tests above: a
    // network-free CI test cannot drive a genuinely LIVE `make_engine` arm end to end, so the
    // pure functions the live call site actually invokes are the CI-safe proof). Every test below
    // was broken (by commenting out the fix under test) and confirmed to fail, then restored,
    // before being trusted — see the task report for the per-test confirmation.
    // ---------------------------------------------------------------------------------------

    fn market_order(symbol: &str, qty: f64) -> vike_model::OrderRequest {
        vike_model::OrderRequest {
            client_order_id: "t".into(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side: 1,
            qty,
            order_type: "market".into(),
            ..Default::default()
        }
    }

    #[test]
    fn arm_universal_defaults_arms_the_throttle_when_absent() {
        let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
        assert_eq!(armed.max_orders_per_window, Some(super::ARMED_MAX_ORDERS_PER_WINDOW));
        assert_eq!(armed.window_ms, 1000, "untouched -> RiskLimits::new()'s own 1000ms default");
        // Issue #822: `max_leverage` is NOT armed here any more (#817's `Some(1.0)` enforced
        // nothing and duplicated the `im_requirement` rescue). It stays whatever reached this fn.
        assert_eq!(armed.max_leverage, None);
        // required_free_bp_pct needs no rescue in this fn: already 0.0 from RiskLimits::new().
        assert_eq!(armed.required_free_bp_pct, 0.0);
    }

    /// The compose-not-fight property (required test 3): an explicit value already present
    /// (mirroring what `merge_operator_budget` would have left after a profile set it) must NOT
    /// be clobbered by the default — `.or(..)` only fills a `None`.
    #[test]
    fn arm_universal_defaults_never_overrides_an_explicit_value() {
        let explicit = vike_exec::RiskLimits {
            max_orders_per_window: Some(7),
            window_ms: 500,
            max_leverage: Some(3.0),
            ..vike_exec::RiskLimits::new()
        };
        let armed = super::arm_universal_defaults(explicit);
        assert_eq!(armed.max_orders_per_window, Some(7), "an explicit value must win");
        assert_eq!(armed.window_ms, 500);
        assert_eq!(armed.max_leverage, Some(3.0), "carried through untouched, never clobbered");
    }

    /// THE HEADLINE test (required test 1, first half): "the field is populated" is not "the
    /// check runs" — build a REAL `RiskGate` straight from the armed defaults (no profile
    /// involved at all) and prove it actually DENIES a rate violation once `ARMED_MAX_ORDERS_PER_WINDOW`
    /// orders have already landed in the window.
    #[test]
    fn armed_defaults_gate_actually_denies_a_rate_violation() {
        let armed = super::arm_universal_defaults(vike_exec::RiskLimits::new());
        let mut gate = vike_exec::RiskGate::new(armed);
        let req = market_order("BTCUSDT", 0.001);
        let ctx = vike_exec::RiskContext {
            mark_price: 100.0,
            equity: 1_000_000.0,
            ..vike_exec::RiskContext::default()
        };
        for i in 0..super::ARMED_MAX_ORDERS_PER_WINDOW {
            let v = gate.check(&req, &ctx);
            assert!(v.ok, "order {i} within the armed per-window cap must pass: {v:?}");
        }
        let v = gate.check(&req, &ctx);
        assert!(!v.ok, "the order beyond the armed per-window cap must be DENIED");
        assert_eq!(v.reason, "rate-limited");
    }

    /// THE HEADLINE test (required test 1, second half) — the `max_leverage` field's honest
    /// story, now with issue #822's resolution folded in. `RiskGate::check` still never evaluates
    /// `RiskLimits::max_leverage` (no production caller of `clamp_leverage` exists either —
    /// verified: `Command::SetMargin` writes `im_by_symbol` directly), so this test does NOT claim
    /// that field denies anything. What enforces "no leverage unless asked" is `im_requirement`,
    /// armed at 1.0 by the rescue one line below this fn's call site in `make_engine`
    /// (`limits.im_requirement = limits.im_requirement.or(Some(1.0))`, pre-existing since PR #816)
    /// — and, since #822, ALSO the destination an operator's `[risk] max_leverage` converts into.
    /// This test proves THAT mechanism actually denies an over-leveraged order, mirroring exactly
    /// what `make_engine` builds. #817's duplicate `max_leverage = 1.0` arming is gone, so the
    /// field is `None` here: an inert knob is no longer populated to look like protection.
    #[test]
    fn armed_leverage_is_enforced_via_im_requirement_not_max_leverage() {
        let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
        assert_eq!(limits.max_leverage, None, "the inert knob is no longer armed");
        limits.im_requirement = limits.im_requirement.or(Some(1.0)); // make_engine's own rescue
        let mut gate = vike_exec::RiskGate::new(limits);
        // 20 units at $100 = $2,000 notional against $1,000 equity at 1x buying power -> denied.
        let req = market_order("BTCUSDT", 20.0);
        let ctx = vike_exec::RiskContext {
            mark_price: 100.0,
            equity: 1_000.0,
            ..vike_exec::RiskContext::default()
        };
        let v = gate.check(&req, &ctx);
        assert!(!v.ok, "an order needing more than 1x buying power must be denied: {v:?}");
        assert_eq!(v.reason, "insufficient-margin");
    }

    /// Required test 4: over-arming, not under-arming, is the real risk of this change — a
    /// perfectly normal order, comfortably inside every armed default, must still pass.
    #[test]
    fn armed_defaults_gate_still_passes_a_normal_order() {
        let mut limits = super::arm_universal_defaults(vike_exec::RiskLimits::new());
        limits.im_requirement = limits.im_requirement.or(Some(1.0));
        let mut gate = vike_exec::RiskGate::new(limits);
        let req = market_order("BTCUSDT", 1.0);
        let ctx = vike_exec::RiskContext {
            mark_price: 100.0,
            equity: 1_000_000.0,
            ..vike_exec::RiskContext::default()
        };
        let v = gate.check(&req, &ctx);
        assert!(v.ok, "a normal order well within every armed default must pass: {v:?}");
    }

    #[test]
    fn require_live_risk_budget_ok_when_both_set() {
        let limits = vike_exec::RiskLimits {
            max_notional_per_order: Some(1.0),
            max_total_exposure: Some(1.0),
            ..vike_exec::RiskLimits::new()
        };
        assert!(super::require_live_risk_budget("binance", &limits, true).is_ok());
    }

    /// Required test 2: a live mount with NEITHER account-dependent cap set must refuse to
    /// start, naming BOTH missing keys in one message (not just the first).
    #[test]
    fn require_live_risk_budget_names_both_missing_keys() {
        let limits = vike_exec::RiskLimits::new(); // neither cap set
        // `false` = no profile reached the mount, the shape that produces the whole-file variant
        // of the diagnostic — which is also the only way BOTH caps go missing in practice.
        let err = super::require_live_risk_budget("okx", &limits, false)
            .expect_err("neither account-dependent cap set -> must refuse to start");
        let msg = format!("{err}");
        assert!(msg.contains("okx"), "error must name the venue: {msg}");
        assert!(msg.contains("max_notional_per_order"), "must name the 1st missing key: {msg}");
        assert!(msg.contains("max_total_exposure"), "must name the 2nd missing key: {msg}");
    }

    /// Only the field that is ACTUALLY missing is named — a supplied cap must not be blamed.
    #[test]
    fn require_live_risk_budget_names_only_the_actually_missing_key() {
        let limits = vike_exec::RiskLimits {
            max_notional_per_order: Some(1.0), // supplied
            ..vike_exec::RiskLimits::new()     // max_total_exposure still None
        };
        // `true` = a profile DID reach the mount — the only way one cap can be set while the other
        // is not — so the diagnostic renders the add-these-lines variant, whose `[risk]` fragment
        // lists exactly the missing keys. (Under `false` the message deliberately prints a COMPLETE
        // minimal profile, both caps included, because there is no file to add a line to.)
        let err = super::require_live_risk_budget("bybit", &limits, true)
            .expect_err("one missing cap is still a refusal");
        let msg = format!("{err}");
        assert!(!msg.contains("max_notional_per_order"), "must not blame the supplied key: {msg}");
        assert!(msg.contains("max_total_exposure"), "must name the actually-missing key: {msg}");
    }

    /// The STARTUP DIAGNOSTIC contract, for the no-profile case — the wall every new user of a
    /// live-mounting binary hits first. The old message named neither the knob nor the fix, so an
    /// operator could only get past it by reading `run_profile.rs`'s `#[cfg(test)]` `LIVE_TOML`
    /// fixture. Each assertion below is one thing that had to be discoverable from source before.
    #[test]
    fn no_profile_diagnostic_names_the_knob_the_keys_and_a_working_profile() {
        let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
            .expect_err("no budget from any source -> refusal");
        let msg = format!("{err}");

        // 1. WHAT is wrong, in words, not a Debug dump.
        assert!(msg.contains("no risk budget"), "states the problem plainly: {msg}");
        assert!(!msg.contains("MissingRiskBudget"), "must not leak the Debug shape: {msg}");
        // 2. WHICH resolver to use. Both, because `resolve_profile`'s precedence is
        //    explicit-flag-beats-env and only some binaries pass an explicit path. Asserted on the
        //    ACTIONABLE spelling (`Set VIKE_RUN_PROFILE=`) rather than the bare name — a stronger
        //    check, and it keeps this library file free of a standalone env-shaped string literal,
        //    which `vike_ops::scan::find_map_lookups` would read as an injected-map env read here.
        assert!(msg.contains("Set VIKE_RUN_PROFILE=<run.toml>"), "names the env var: {msg}");
        assert!(msg.contains("--profile <run.toml>"), "names the flag: {msg}");
        // 3. WHICH keys — the error already knew them; it just never printed them usefully.
        assert!(msg.contains("max_notional_per_order"), "names the 1st key: {msg}");
        assert!(msg.contains("max_total_exposure"), "names the 2nd key: {msg}");
        // 4. The message IS the example: a `[risk]` table, plus the sections the schema requires
        //    (`mode`/`[event_source]`/`[broker]` are NOT `#[serde(default)]` on `RunProfile`, so a
        //    `[risk]`-only file would earn a parse error instead of a working mount).
        assert!(msg.contains("[risk]"), "shows a [risk] table: {msg}");
        assert!(msg.contains("mode = \"live\""), "shows the mode the live mount demands: {msg}");
        assert!(msg.contains("[event_source]"), "shows the required event source: {msg}");
        assert!(msg.contains("[broker]"), "shows the required broker: {msg}");
        // 5. WHERE the fuller template lives, so the message is not the only copy.
        assert!(msg.contains(super::EXAMPLE_PROFILE_PATH), "names the shipped template: {msg}");
        // 6. The venue is threaded into the example so it is copy-pasteable as printed.
        assert!(msg.contains("venue  = \"binance\""), "example names the refusing venue: {msg}");
    }

    /// The OTHER half of the two-case split: a profile exists but omits a cap. The fix is an edit,
    /// not a new file, so the message must NOT print a whole profile (which an operator would
    /// reasonably paste over the file they already have, losing the rest of their config).
    #[test]
    fn supplied_profile_diagnostic_asks_for_an_edit_not_a_new_file() {
        let limits = vike_exec::RiskLimits {
            max_notional_per_order: Some(1.0),
            ..vike_exec::RiskLimits::new()
        };
        let msg = format!(
            "{}",
            super::require_live_risk_budget("okx", &limits, true).expect_err("still a refusal")
        );
        assert!(msg.contains("[risk]"), "still shows the table to edit: {msg}");
        assert!(msg.contains("max_total_exposure = 25000.0"), "shows a usable value: {msg}");
        assert!(!msg.contains("[broker]"), "must not print a whole replacement profile: {msg}");
        assert!(
            !msg.contains("mode = \"live\""),
            "must not print a whole replacement profile: {msg}"
        );
        assert!(msg.contains("VIKE_RUN_PROFILE / --profile"), "says which file to edit: {msg}");
    }

    /// [`super::BUDGET_EXAMPLES`] must cover every key [`super::require_live_risk_budget`] can
    /// report. A third account-dependent cap added there without a row here would silently vanish
    /// from the inline `[risk]` example — the message would name a key in its `missing:` line and
    /// then fail to show how to set it, which is exactly the discoverability hole this work closed.
    #[test]
    fn every_missing_key_has_an_example() {
        let err = super::require_live_risk_budget("binance", &vike_exec::RiskLimits::new(), false)
            .expect_err("no budget -> refusal naming every reportable key");
        let missing = match err {
            super::MountError::MissingRiskBudget { missing, .. } => missing,
        };
        for key in &missing {
            assert!(
                super::BUDGET_EXAMPLES.iter().any(|(k, _, _)| k == key),
                "`{key}` is reportable but has no BUDGET_EXAMPLES row, so the diagnostic cannot \
                 show how to set it"
            );
        }
    }

    /// Required test 5 (the make_engine-level proof): a PAPER mount — the arm every venue in
    /// `all_roster_venues_absent_creds_stay_paper_and_inert` takes with an empty `.env` — succeeds
    /// with `Ok` even though NEITHER account-dependent cap was ever supplied (no risk_profile at
    /// all). `require_live_risk_budget` is gated on `live_venues.contains(venue)` at the
    /// `make_engine` call site, which stays empty on the paper path, so the refusal never fires.
    #[test]
    fn paper_mount_starts_with_no_account_dependent_budget_at_all() {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let (engine, _recon) = super::make_engine(
            "binance",
            "BTCUSDT",
            &std::collections::HashMap::new(), // no creds -> paper
            &tx,
            &mut live,
            false, // reconcile off — this test is about the risk budget, not recon
            None,
            None,
            None, // no operator risk_profile either
            // ⚠ binance is ARMED by the ceiling here on purpose: this test's subject is that
            // ABSENT CREDENTIALS keep the budget refusal from firing, and a `paper` ceiling would
            // reach the same `Ok` one step earlier, without the refusal's own gate being consulted.
            Some(&super::armed_policy("binance", vike_config::VenueMode::Demo)),
        )
        .expect("a paper mount must never refuse to start over an unset account-dependent cap");
        assert_eq!(engine.gate.limits.max_notional_per_order, None);
        assert_eq!(engine.gate.limits.max_total_exposure, None);
    }
}

/// The GLOBAL reconcile gate (`make_engine`'s `recon_enabled`), proven BOTH ways with no network
/// and no credentials.
///
/// The five arms it gates — deribit / ctrader / ig / oanda / ibkr — each build their `ReconClient`
/// with a BLOCKING AUTHENTICATED handshake, so the arms themselves cannot be exercised in either
/// direction here (that needs real credentials AND a reachable venue, which is what the `#[ignore]`d
/// reconcile smokes are for). The decision is therefore extracted into `recon_if_enabled`,
/// which IS pure, and pinned here — the same "extract the decision, test the decision" shape aster's
/// `range_target` uses.
///
/// What matters is the LAZINESS, not the returned `Option`: absent credentials, an unreachable venue
/// and a disabled gate all produce `None`, so asserting on the result alone would have stayed green
/// against the very bug this closes (build the client, throw it away). The counting closure below is
/// what actually distinguishes them.
#[cfg(test)]
mod recon_gate_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vike_exec::recon::{FakeReconClient, ReconClient};

    /// The workspace's existing offline `ReconClient` double — never fetched from here; these tests
    /// assert on WHETHER a client was constructed, never on what it returns.
    fn stub() -> Box<dyn ReconClient> {
        Box::new(FakeReconClient::default())
    }

    /// GATE OFF — THE bug this closes: the factory is not merely ignored, it is never CALLED, so no
    /// blocking authed handshake is issued at mount when `VIKE_RECONCILE` is unset. The `calls`
    /// counter is the assertion that matters; the `None` alone would be satisfied by the old
    /// (build-then-discard) code too.
    #[test]
    fn recon_if_enabled_is_lazy_when_disabled() {
        let calls = AtomicUsize::new(0);
        let out = super::recon_if_enabled(false, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(stub())
        });
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "disabled ⇒ the venue's factory must NEVER run"
        );
        assert!(out.is_none(), "disabled ⇒ the venue stays reconcile-inert, like a paper venue");
    }

    /// GATE ON — the other direction, and the one that keeps the fix from being a silent
    /// reconciliation outage: the factory runs EXACTLY once and its result is passed through
    /// untouched.
    #[test]
    fn recon_if_enabled_builds_exactly_once_when_enabled() {
        let calls = AtomicUsize::new(0);
        let out = super::recon_if_enabled(true, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(stub())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1, "enabled ⇒ built once, not zero and not twice");
        assert!(out.is_some(), "enabled ⇒ the factory's client is returned verbatim");
    }

    /// …and an ENABLED gate does not paper over a factory that fails: a venue whose handshake
    /// returns `None` (unreachable / bad creds) still resolves `None`, exactly as before the gate —
    /// the gate short-circuits, it never substitutes.
    #[test]
    fn recon_if_enabled_passes_through_a_failed_build() {
        let calls = AtomicUsize::new(0);
        let out = super::recon_if_enabled(true, || {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(out.is_none(), "a failed handshake stays reconcile-inert, exec unaffected");
    }

    /// POLYMARKET'S OWN COMPOSITION, all four rows — the venue that is gated TWICE, pinned as a
    /// matrix so neither gate can be dropped without a red test.
    ///
    /// The whole matrix is the assertion rather than the one `true` row: dropping the master gate
    /// (the shipped spelling until 2026-09-06, `poly_reconcile` alone) leaves row 2 green and only
    /// row 3 red, and dropping the venue gate leaves row 3 green and only row 2 red. Asserting the
    /// `true` row alone would pass with either gate removed, which is the shape of test this
    /// workspace treats as a bug.
    ///
    /// It is a pure function precisely because the real arm cannot be exercised in either
    /// direction: reaching it needs a real Polygon key and a reachable CLOB (the same reason
    /// `recon_if_enabled` above is extracted). What CANNOT be asserted here is that the ARM calls
    /// it — that rests on review, and on `polymarket_without_the_gates_is_inert_and_offline`, which
    /// runs the real arm with the master gate ON and both venue gates off.
    #[cfg(feature = "polymarket")]
    #[test]
    fn polymarket_wants_a_recon_client_only_when_both_gates_are_on() {
        assert!(
            super::poly_recon_wanted(true, true),
            "master gate on + POLY_RECONCILE=1 ⇒ the venue's reconcile client is built — since S2 \
             the master gate is on by DEFAULT for a live mount, so this is the row a box carrying \
             POLY_RECONCILE=1 and no VIKE_RECONCILE now takes"
        );
        assert!(
            !super::poly_recon_wanted(true, false),
            "the venue gate is still an act nobody else needs: no POLY_RECONCILE ⇒ Polymarket \
             reconciles nothing, whatever the master gate says"
        );
        assert!(
            !super::poly_recon_wanted(false, true),
            "THE REGRESSION ROW: a refused or paper mount (VIKE_RECONCILE_OFF=1, or nothing armed \
             live) must do NO authenticated Polymarket work — the arm used to build the client \
             here anyway and let the driver-less root drop it"
        );
        assert!(!super::poly_recon_wanted(false, false), "neither gate ⇒ nothing, as before");
    }
}

/// PRE-CONNECT live-intent probe + refusal (the #817 "refusal happens POST-connect" residual):
/// `would_mount_live` must recognize exactly the live arms' own credential shapes — via the SAME
/// loaders the arms call — and the budget refusal must fire on it BEFORE any venue session.
#[cfg(test)]
mod preconnect_tests {
    use std::collections::HashMap;
    use vike_model::account_keys::AccountLabel;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A bybit store holding the DEFAULT account's demo key set and a LABELLED `ALT` one beside it.
    ///
    /// ⚠ The labelled key NAMES are rendered by `vike_model::account_keys::account_key` rather than
    /// written out, and that is not style. `crates/vike-ops/tests/settings_registry.rs` harvests
    /// credential-key LITERALS out of every `.rs` file under `crates/*/src/` and demands a
    /// `vike_ops::settings::SETTINGS` row for each; a `__ALT` spelling has no row and can never have
    /// one, because the label is a name the OPERATOR chooses at runtime. Building the key through
    /// the grammar's own renderer keeps the literal out of the source AND makes this fixture wrong
    /// the day the separator changes.
    fn bybit_store_with_alt() -> HashMap<String, String> {
        use vike_model::account_keys::account_key;
        let alt = AccountLabel::parse("ALT").expect("a legal label");
        let mut out = HashMap::new();
        for (base, value) in [("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")] {
            out.insert(base.to_string(), value.to_string());
            out.insert(account_key(base, &alt), format!("{value}2"));
        }
        out
    }

    /// Roster-wide inert default (the capability-map playbook's completeness direction): with NO
    /// credentials of any shape, EVERY roster venue probes paper — a mount with an empty vars map
    /// must never be refused over a risk budget it cannot need.
    #[test]
    fn empty_vars_probe_false_for_every_roster_venue() {
        for v in vike_model::VENUES {
            assert!(
                !super::would_mount_live(v, &HashMap::new()),
                "{v} must probe paper with no creds"
            );
        }
    }

    /// One `(venue, its live arm's own credential shape)` row per venue with a live arm in a
    /// DEFAULT build — the var names are the arms' own loaders' names (fixture-pinned in each
    /// bridge crate).
    ///
    /// Hoisted out of `probe_recognizes_each_live_arms_cred_shape` because the arming-ceiling
    /// suite below needs exactly the same maps for the opposite claim: those tests assert that a
    /// `paper` ceiling refuses a mount these maps WOULD have armed, and a table of their own would
    /// let the two drift until the ceiling suite was silently testing unarmed venues.
    /// `every_roster_venue_is_a_live_arm_row_or_has_no_live_arm` is the completeness half.
    fn live_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
        &[
            ("binance", &[("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")]),
            ("bybit", &[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")]),
            (
                "okx",
                &[
                    ("OKX_DEMO_API_KEY", "k"),
                    ("OKX_DEMO_API_SECRET", "s"),
                    ("OKX_DEMO_API_PASSPHRASE", "p"),
                ],
            ),
            ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
            ("aster", &[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]),
            ("hyperliquid", &[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xkey")]),
            (
                "ctrader",
                &[
                    ("CTRADER_CLIENT_ID", "id"),
                    ("CTRADER_CLIENT_SECRET", "sec"),
                    ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
                    ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
                ],
            ),
            (
                "alpaca",
                &[
                    ("ALPACA_SANDBOX_CLIENT_ID", "id"),
                    ("ALPACA_SANDBOX_CLIENT_SECRET", "sec"),
                    ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
                ],
            ),
            (
                "ig",
                &[("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "u"), ("IG_DEMO_PASSWORD", "p")],
            ),
            ("oanda", &[("OANDA_DEMO_API_KEY", "k"), ("OANDA_DEMO_ACCOUNT_ID", "a")]),
        ]
    }

    /// Each live arm's credential shape flips its probe row to `true` — so a loader renaming its
    /// keys, or this probe drifting off its arm's loader, fails by name.
    #[test]
    fn probe_recognizes_each_live_arms_cred_shape() {
        for (venue, kv) in live_arming_cases() {
            assert!(
                super::would_mount_live(venue, &vars(kv)),
                "{venue}: its live arm's cred shape must probe live"
            );
        }
    }

    /// The venues [`live_arming_cases`] deliberately has NO row for, each with the reason — the
    /// not-applicable column of the same table, which is what makes the partition below a
    /// completeness gate rather than a length check.
    ///
    /// Every one of them is a venue no DEFAULT build can mount live from a credential map alone, so
    /// there is no map on which `would_mount_live` is true and nothing for the arming-ceiling suite
    /// to refuse. `venues_without_a_live_arm_probe_false_even_with_creds` and the two fxcm tests
    /// drive the individual cases with their real key shapes, under BOTH feature states.
    const NO_LIVE_ARM: &[(&str, &str)] = &[
        ("dukascopy", "ships an exec factory, and `make_engine` has no arm that mounts it"),
        ("fxcm", "arm exists only under vike-mount's `fxcm` feature AND with a linked SDK"),
        ("ibkr", "arm exists only under vike-mount's `ibkr` feature"),
        ("polymarket", "arm exists only under the `polymarket` feature, and needs POLY_EXEC=1"),
    ];

    /// **The COMPLETENESS gate over [`live_arming_cases`]**, in the shape every per-venue
    /// capability table uses: the armed rows and the declared not-applicable rows must partition
    /// `vike_model::VENUES` exactly — no venue in both, no venue in neither.
    ///
    /// A thirteenth bridge therefore reddens this until its author says which column it is in, and
    /// a venue that GAINS a live arm cannot stay silently uncovered by the arming-ceiling suite
    /// below (the failure that matters: that suite proves a `paper` ceiling refuses an arming, and
    /// a venue with no row is a venue it never tries to refuse).
    #[test]
    fn every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable() {
        let mut rows: Vec<&str> = live_arming_cases().iter().map(|(v, _)| *v).collect();
        let excused: Vec<&str> = NO_LIVE_ARM.iter().map(|(v, _)| *v).collect();
        for (venue, why) in NO_LIVE_ARM {
            assert!(why.len() > 20, "{venue}'s exemption must carry a REASON, got {why:?}");
            assert!(!rows.contains(venue), "{venue} is in BOTH columns");
        }
        rows.extend(&excused);
        rows.sort_unstable();
        let mut roster = vike_model::VENUES.to_vec();
        roster.sort_unstable();
        assert_eq!(
            rows, roster,
            "`live_arming_cases` + NO_LIVE_ARM must be exactly the roster: a new venue needs its \
             live arm's credential shape in the first, or a written reason in the second"
        );
    }

    /// **THE HALF-CREDENTIAL GATE, at the mount.** OKX's signer sends `OK-ACCESS-PASSPHRASE` on
    /// every request (`vike_bridge_core::venue_passphrase`'s `venue_passphrase` row), so a store
    /// holding key + secret and NO `OKX_{TIER}_API_PASSPHRASE` is UNUSABLE — and until this gate it
    /// LOADED: `("okx", Some(c))` matched, the venue was marked live, an exec actor was spawned,
    /// and every signed request came back rejected by the venue. Half credentials must reach the
    /// same verdict as absent ones, because absent credentials ARE the live gate.
    ///
    /// Asserted at BOTH gates that decide it, since either alone could regress independently: the
    /// pure pre-connect probe, and a REAL `make_engine` mount (which stays offline precisely
    /// because the gate holds — a regression makes this test dial OKX).
    #[test]
    fn okx_key_and_secret_without_a_passphrase_do_not_mount_live() {
        let half = vars(&[("OKX_DEMO_API_KEY", "k"), ("OKX_DEMO_API_SECRET", "s")]);
        assert!(
            !super::would_mount_live("okx", &half),
            "okx with no passphrase must probe PAPER — its key+secret cannot sign anything"
        );

        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let (engine, recon) = super::make_engine(
            "okx",
            "BTC-USDT-SWAP",
            &half,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            // ⚠ okx is ARMED by the ceiling: the gate under test is the missing PASSPHRASE, and a
            // `paper` ceiling would produce the same paper mount one step earlier — making every
            // assertion below true for the wrong reason.
            Some(&super::armed_policy("okx", vike_config::VenueMode::Demo)),
        )
        .expect("a paper mount must never refuse to start");
        assert!(live.is_empty(), "half credentials must NOT mark okx live");
        assert!(recon.is_none(), "a paper venue never reconciles");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_model::fee_schedule_for("okx")),
            "the paper mount is tagged with okx's static fee schedule"
        );

        // The CONTROL: the same map plus the passphrase flips the probe live, so this test pins the
        // PASSPHRASE as the gate and not some unrelated okx breakage. (Only the pure probe is
        // exercised on that side — `make_engine` with complete credentials would dial the venue.)
        let mut full = half.clone();
        full.insert("OKX_DEMO_API_PASSPHRASE".to_string(), "p".to_string());
        assert!(
            super::would_mount_live("okx", &full),
            "with all three credentials okx probes live — the gate is the passphrase, nothing else"
        );
    }

    /// **THE OANDA LIVE-TIER REFUSAL, at the mount.** `vike_oanda::oanda_hosts` implements and
    /// tests the fxTrade tier and no caller in this workspace ever asks for it, so a store holding
    /// `OANDA_LIVE_*` used to mount PAPER in silence — and a store holding BOTH tiers used to place
    /// REAL orders on the practice account while the operator believed their live keys were in
    /// force. The arm now refuses to select ANY tier from a live-armed store, the practice fallback
    /// included, and says so at `error!`.
    ///
    /// Asserted at BOTH gates that decide it, the same shape as the OKX passphrase gate above: the
    /// pure pre-connect probe, and a REAL `make_engine` mount — which stays offline precisely
    /// because the refusal holds. A regression makes this test dial OANDA's practice account with
    /// the demo token below, which is the behaviour being pinned out.
    #[test]
    fn an_oanda_live_key_set_refuses_the_mount_instead_of_trading_the_practice_account() {
        let armed = vars(&[
            ("OANDA_LIVE_API_KEY", "live-tok"),
            ("OANDA_LIVE_ACCOUNT_ID", "001-001-0000001-001"),
            ("OANDA_DEMO_API_KEY", "demo-tok"),
            ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        assert!(
            !super::would_mount_live("oanda", &armed),
            "a live-armed oanda store must probe PAPER — the arm refuses it, so live INTENT here \
             would raise a budget refusal over a venue that can only be paper"
        );

        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        // ⚠ oanda is ARMED by the ceiling: the gate under test is the arm's own refusal of a
        // LIVE-named key set, and a `paper` ceiling would reach the same paper mount one step
        // earlier — making every assertion below true for the wrong reason.
        let permitted = super::armed_policy("oanda", vike_config::VenueMode::Demo);
        let (engine, recon) = super::make_engine(
            "oanda",
            "EUR_USD",
            &armed,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            Some(&permitted),
        )
        .expect("a paper mount must never refuse to start");
        assert!(live.is_empty(), "a live-armed oanda store must NOT mount live on practice");
        assert!(recon.is_none(), "a paper venue never reconciles");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_model::fee_schedule_for("oanda")),
            "the paper mount is tagged with oanda's static fee schedule"
        );

        // The CONTROL: the SAME map with the live pair removed probes live again, so this test
        // pins the live-named key set as the gate and not some unrelated oanda breakage. (Only the
        // pure probe is exercised on that side — `make_engine` with practice credentials would
        // dial the venue.)
        let practice = vars(&[
            ("OANDA_DEMO_API_KEY", "demo-tok"),
            ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        assert!(
            super::would_mount_live("oanda", &practice),
            "practice credentials alone still probe live — the gate is the live-named set, \
             nothing else"
        );
    }

    /// The MIRROR of the gate above, and the reason it is a per-venue TABLE rather than a blanket
    /// rule: binance/bybit/deribit sign with key + secret alone, so requiring a passphrase
    /// everywhere would silently strand every working mount on paper — the same defect wearing the
    /// opposite sign. Their probes must stay live with exactly two credentials.
    #[test]
    fn passphrase_free_venues_still_probe_live_without_one() {
        for (venue, kv) in [
            ("binance", &[("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")]),
            ("bybit", &[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")]),
            ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
        ] {
            assert!(
                super::would_mount_live(venue, &vars(kv)),
                "{venue} takes no passphrase — key+secret alone must still probe live"
            );
        }
    }

    /// FXCM credentials for the probe tests below — the `load_fxcm_config_from(Demo, …)` shape the
    /// arm itself gates on.
    fn fxcm_creds() -> HashMap<String, String> {
        vars(&[("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "p")])
    }

    /// ⚠ **THIS PIN CHANGED.** It used to read
    /// `assert!(!would_mount_live("fxcm", &fx))` unconditionally, in the test below, on the
    /// grounds that `make_engine` had no fxcm arm. It has one now, so the flat `false` is gone and
    /// the answer is a conjunction of two facts — and the SDK one is unreachable from any CI box,
    /// which is why the pure [`fxcm_live_intent`] carries it as a parameter and this test drives
    /// BOTH values of it.
    ///
    /// The end-to-end `would_mount_live` answer is asserted against `sdk_linked()` rather than
    /// against a literal: on CI that is `false` (no runner has ever linked ForexConnect) and on a
    /// developer box with the SDK staged it is `true`, and the test must be honest on both.
    #[cfg(feature = "fxcm")]
    #[test]
    fn fxcm_probes_live_only_with_credentials_and_a_linked_sdk() {
        let fx = fxcm_creds();

        // The pure decision, both directions. A stub build REFUSES the live mount (its exec client
        // would accept orders and discard them), so live intent there would raise a budget refusal
        // over a venue that can only be paper — the same stance the oanda live-tier row takes.
        assert!(
            super::fxcm_live_intent(true, &AccountLabel::Default, &fx),
            "credentials + a linked SDK is the one combination the arm mounts live"
        );
        assert!(
            !super::fxcm_live_intent(false, &AccountLabel::Default, &fx),
            "a STUB build's arm refuses and lands on paper, so it must not report live intent"
        );
        assert!(
            !super::fxcm_live_intent(true, &AccountLabel::Default, &HashMap::new()),
            "a linked SDK with no credentials is the ordinary unconfigured state — absent \
             credentials ARE the live gate"
        );
        // Half a credential is not a credential: `load_fxcm_config_from` needs BOTH.
        assert!(!super::fxcm_live_intent(
            true,
            &AccountLabel::Default,
            &vars(&[("FXCM_DEMO_USER", "u")]),
        ));
        assert!(!super::fxcm_live_intent(
            true,
            &AccountLabel::Default,
            &vars(&[("FXCM_DEMO_PASSWORD", "p")]),
        ));

        // …and the wired probe agrees with the pure one at THIS binary's linkage.
        assert_eq!(
            super::would_mount_live("fxcm", &fx),
            vike_fxcm::sdk_linked(),
            "with credentials present the probe is exactly the SDK question"
        );
    }

    /// The other half of the pin: with the feature OFF — the default build, and the one every
    /// non-`fxcm` CI lane compiles — the arm does not exist and fxcm can only reach the paper `_`
    /// arm, so it probes false with credentials present, exactly as before.
    #[cfg(not(feature = "fxcm"))]
    #[test]
    fn fxcm_probes_false_with_the_feature_off_even_with_creds() {
        assert!(
            !super::would_mount_live("fxcm", &fxcm_creds()),
            "no feature ⇒ no arm ⇒ paper, whatever the store says"
        );
    }

    /// Venues with NO live `make_engine` arm probe paper even when their own credential shapes
    /// are present — dukascopy ships an exec factory, but nothing in `make_engine` mounts it,
    /// so a budget refusal over it would block a mount that can only ever be paper.
    ///
    /// ⚠ fxcm USED TO BE IN THIS TEST and is not any more: it grew an arm. Its replacement is the
    /// pair of tests above, which assert the same `false` under the default build and a real
    /// two-fact conjunction under the feature.
    #[test]
    fn venues_without_a_live_arm_probe_false_even_with_creds() {
        let duka = vars(&[("DUKASCOPY_DEMO1_LOGIN", "u"), ("DUKASCOPY_DEMO1_PASSWORD", "p")]);
        assert!(!super::would_mount_live("dukascopy", &duka));
        // polymarket's arm exists only under the feature, and intent needs BOTH factory gates:
        // the explicit POLY_EXEC=1 flag AND key material. The flag ALONE can never mount live
        // (no key ⇒ the factory returns before any network) and must stay paper-probed — the
        // contract `polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline` pins.
        assert!(!super::would_mount_live("polymarket", &vars(&[("POLY_EXEC", "1")])));
        #[cfg(feature = "polymarket")]
        assert!(super::would_mount_live(
            "polymarket",
            &vars(&[("POLY_EXEC", "1"), ("POLY_PRIVATE_KEY", "0xkey")])
        ));
        #[cfg(not(feature = "polymarket"))]
        assert!(!super::would_mount_live(
            "polymarket",
            &vars(&[("POLY_EXEC", "1"), ("POLY_PRIVATE_KEY", "0xkey")])
        ));
    }

    /// The pre-connect PREVIEW equals the post-merge verdict for the two budget caps: a profile
    /// supplying both passes the same `require_live_risk_budget` the refusal calls; one missing
    /// either is refused naming it. (`to_risk_limits` is the preview the `make_engine` site
    /// builds; the two caps are operator-owned on every merge path, so preview == final.)
    #[test]
    fn preview_budget_matches_the_gate_verdict() {
        let full = vike_exec::ProfileRisk {
            max_notional_per_order: Some(100.0),
            max_total_exposure: Some(500.0),
            ..Default::default()
        };
        assert!(super::require_live_risk_budget("binance", &full.to_risk_limits(), true).is_ok());
        let half =
            vike_exec::ProfileRisk { max_notional_per_order: Some(100.0), ..Default::default() };
        let err = super::require_live_risk_budget("binance", &half.to_risk_limits(), true)
            .expect_err("one missing cap must refuse");
        assert!(format!("{err}").contains("max_total_exposure"));
    }

    // ===========================================================================================
    // THE ARMING CEILING (settings-unification stage 3)
    //
    // ⚠ Every test below drives the REAL `make_engine_with_legs` with REAL live-arming credential
    // shapes, and every one of them stays OFFLINE — because the ceiling holds. A regression makes
    // this suite dial ten venues, which is the same stance (and the same wording) as the OKX
    // passphrase and OANDA live-tier gates above.
    //
    // ⚠ The LIVE side is asserted through the pre-connect BUDGET REFUSAL rather than through
    // `live_venues`, and that is not a weaker check dressed up — it is the only observable a
    // network-free test HAS. Reaching `live_venues.insert` means the venue's arm ran, i.e. a
    // blocking instrument fetch and a dialing exec thread; `MountError::MissingRiskBudget` fires
    // one step earlier, from `would_mount_live_under`, and firing it PROVES the mount classified
    // the venue as live-intent under that ceiling. The paper side, where nothing dials, is asserted
    // on `live_venues` directly.
    // ===========================================================================================

    use vike_config::{VenueMode, VenuePolicy};

    /// A `MountPolicy` declaring exactly one venue's ceiling — every other venue keeps `paper`.
    fn ceiling(venue: &str, mode: VenueMode) -> crate::MountPolicy {
        crate::MountPolicy {
            venues: VenuePolicy::default().declare(venue, mode),
            ..crate::MountPolicy::default()
        }
    }

    /// Mount `venue` for real, with no risk profile and no recorder, and report both halves: the
    /// result and the live set the mount wrote into.
    fn mount(
        venue: &str,
        vars: &HashMap<String, String>,
        policy: Option<&crate::MountPolicy>,
    ) -> (Result<crate::EngineAndRecon, crate::MountError>, std::collections::HashSet<String>) {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = std::collections::HashSet::new();
        let out = super::make_engine_with_legs(
            venue,
            "BTCUSDT",
            &[],
            vars,
            &tx,
            &mut live,
            true,
            None,
            None,
            None,
            policy,
        );
        (out, live)
    }

    /// **THE STAGE-3 PROPERTY, over the WHOLE roster**: a venue whose ceiling is `paper` never gets
    /// a live exec client, no matter what its credentials say — and the very same credentials under
    /// a `live` ceiling do reach the live path, so the refusal is the ceiling and not a broken
    /// fixture.
    ///
    /// ⚠ The `None`-policy leg is not redundant with the `MountPolicy::default()` one, and it is
    /// the leg that catches the fail-safe default being weakened: `policy.map_or(Paper, …)` and
    /// `policy.map_or(Live, …)` agree on every `Some`, and differ only here.
    #[test]
    fn a_paper_moded_venue_never_gets_a_live_exec_client() {
        for (venue, kv) in live_arming_cases() {
            let armed = vars(kv);
            // ANTI-VACUITY: these credentials really would arm this venue. Without this line every
            // assertion below could pass against an empty map.
            assert!(
                super::would_mount_live(venue, &armed),
                "{venue}: the fixture must be a map that ARMS, or this test proves nothing"
            );

            let no_file = crate::MountPolicy::default();
            for policy in [None, Some(&no_file)] {
                let (out, live) = mount(venue, &armed, policy);
                let (engine, recon) = out.unwrap_or_else(|e| {
                    panic!("{venue}: a capped mount is PAPER and must never refuse to start: {e}")
                });
                assert!(
                    live.is_empty(),
                    "{venue}: a `paper` ceiling let a live exec client be armed (policy \
                     supplied: {})",
                    policy.is_some()
                );
                assert!(recon.is_none(), "{venue}: a paper venue never gets a reconcile handle");
                assert_eq!(
                    engine.fee_schedule,
                    Some(vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT"))),
                    "{venue}: the capped mount is still tagged with the venue's fee schedule"
                );
            }
            assert!(
                !super::would_mount_live_under(venue, &armed, VenueMode::Paper),
                "{venue}: a disarmed venue has no live arm to probe"
            );

            // …and the CONTROL, which is what makes the refusals above the CEILING's doing: the
            // same map under `live` reaches the live path. Observed as the pre-connect budget
            // refusal — no venue session is established, so this stays offline.
            assert!(
                super::would_mount_live_under(venue, &armed, VenueMode::Live),
                "{venue}: the same credentials under a `live` ceiling must probe live"
            );
            let permitted = ceiling(venue, VenueMode::Live);
            match mount(venue, &armed, Some(&permitted)).0 {
                Err(crate::MountError::MissingRiskBudget { venue: refused, .. }) => {
                    assert_eq!(refused.as_str(), *venue, "the refusal must name the venue")
                }
                Ok(_) => panic!(
                    "{venue}: a `live` ceiling over live credentials did NOT reach the live path — \
                     the pre-connect budget refusal never fired, so the ceiling is refusing \
                     everything and the paper assertions above prove nothing"
                ),
            }

            // EXACTLY that venue: the same one-venue `live` policy leaves every OTHER armed venue
            // on paper, so a ceiling is per-venue rather than a global switch.
            for (other, other_kv) in live_arming_cases() {
                if other == venue {
                    continue;
                }
                let (out, live) = mount(other, &vars(other_kv), Some(&permitted));
                out.unwrap_or_else(|e| panic!("{other} is capped and must not refuse: {e}"));
                assert!(live.is_empty(), "{venue}=live must not arm {other}");
            }
        }
    }

    /// **`demo` REFUSES THE MAINNET SWITCH.** `BINANCE_MAINNET=1` with only LIVE keys present used
    /// to be the whole gate; under a `demo` ceiling the flag selects nothing, the DEMO tier is
    /// resolved, no demo keys exist, and the venue stays PAPER — never a mainnet host signed with
    /// the live key set the operator's own file just declined.
    ///
    /// The flag is set in the VARS map rather than exported, deliberately: `cex_mainnet_enabled`
    /// reads the process env as well, and a test that mutated global env would be the stray-flag
    /// hazard `only_cex_venues_have_a_mainnet_cred_switch` documents.
    #[test]
    fn demo_mode_refuses_the_mainnet_switch() {
        let armed = vars(&[
            ("BINANCE_MAINNET", "1"),
            ("BINANCE_LIVE_API_KEY", "k"),
            ("BINANCE_LIVE_API_SECRET", "s"),
        ]);
        // CONTROL FIRST, so the refusal below cannot be a fixture that never armed anything: under
        // `live` this exact map reaches the live path (the pre-connect budget refusal fires).
        assert!(super::would_mount_live_under("binance", &armed, VenueMode::Live));
        assert!(matches!(
            mount("binance", &armed, Some(&ceiling("binance", VenueMode::Live))).0,
            Err(crate::MountError::MissingRiskBudget { .. })
        ));

        // …and under `demo` the same map arms nothing at all.
        assert!(
            !super::would_mount_live_under("binance", &armed, VenueMode::Demo),
            "a `demo` ceiling resolves the DEMO tier, and there are no demo keys here"
        );
        let (out, live) = mount("binance", &armed, Some(&ceiling("binance", VenueMode::Demo)));
        out.expect("a demo-capped mainnet-flagged venue is PAPER, and paper never refuses");
        assert!(live.is_empty(), "BINANCE_MAINNET=1 armed a venue the ceiling capped at `demo`");
    }

    /// **THE MEASURED HOLE.** Aster is SWITCHLESS — `vike_bridge_core::mainnet::mainnet_switch_for`
    /// declares it so, there is no `ASTER_MAINNET` for `vike_config::CREDENTIAL_FILE_ARMING_REFUSED`
    /// to carry a row for, and its arm tried `Environment::Live` FIRST. An `ASTER_LIVE_*` pair in
    /// `secrets.env` was therefore by itself an authenticated MAINNET session on a daemon that
    /// never asked for one (observed on the CI box, inside a nine-venue live set).
    ///
    /// Under anything below `live` the Live attempt is DELETED from the chain, so the pair resolves
    /// nothing and the venue is paper.
    #[test]
    fn demo_mode_deletes_the_live_first_attempt_on_a_switchless_venue() {
        let armed = vars(&[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]);
        // CONTROL: the pair genuinely arms this venue under `live` — mainnet, first attempt, today.
        assert!(super::would_mount_live_under("aster", &armed, VenueMode::Live));
        assert!(matches!(
            mount("aster", &armed, Some(&ceiling("aster", VenueMode::Live))).0,
            Err(crate::MountError::MissingRiskBudget { .. })
        ));

        for capped in [VenueMode::Demo, VenueMode::Paper] {
            assert!(
                !super::would_mount_live_under("aster", &armed, capped),
                "{capped}: LIVE aster credentials must arm nothing — there is no flag to refuse \
                 them, so the ceiling is the only refusal that exists"
            );
            let (out, live) = mount("aster", &armed, Some(&ceiling("aster", capped)));
            out.expect("a capped aster mount is PAPER, and paper never refuses to start");
            assert!(live.is_empty(), "{capped}: ASTER_LIVE_* armed a real mainnet account");
        }

        // …and the TESTNET pair still arms under `demo`, so what the ceiling deleted is the LIVE
        // attempt and not the venue.
        let testnet =
            vars(&[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]);
        assert!(super::would_mount_live_under("aster", &testnet, VenueMode::Demo));
    }

    /// A capped venue must be the SAME engine an uncredentialled one gets — the ceiling refuses an
    /// arming, it does not invent a third mode.
    ///
    /// This is the guard on `paper_engine` being a separate assembly from the post-match tail: the
    /// two are compared by OUTPUT (`vike_exec::state_hash` over the real snapshot, plus the fee
    /// schedule, which the snapshot does not carry), so a step added to the tail and not here is
    /// caught rather than merely commented about.
    #[test]
    fn a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets() {
        for (venue, kv) in live_arming_cases() {
            let capped = mount(venue, &vars(kv), Some(&crate::MountPolicy::default()))
                .0
                .unwrap_or_else(|e| panic!("{venue} capped: {e}"))
                .0;
            // The SAME venue with an EMPTY credential map, and a ceiling that permits everything —
            // so the only reason it is paper is the pre-existing absent-credentials gate.
            let unarmed = mount(venue, &HashMap::new(), Some(&ceiling(venue, VenueMode::Live)))
                .0
                .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
                .0;
            assert_eq!(
                vike_exec::state_hash(&[capped.snapshot_state()]),
                vike_exec::state_hash(&[unarmed.snapshot_state()]),
                "{venue}: the capped mount and the uncredentialled one are different engines"
            );
            assert_eq!(capped.fee_schedule, unarmed.fee_schedule, "{venue}");
        }
    }

    // -- the migration warning ------------------------------------------------------------------

    /// **The upgrade warning fires exactly once per box that needs it, and is silent otherwise.**
    ///
    /// Four cases, and the third is the one that matters: an operator who wrote `[venues]` with
    /// EVERY venue at `paper` has stated their arming, and a warning that keeps firing at them is
    /// the "refusal list that fires on harmless lines" `vike_config::arming` names as the way
    /// operators are taught to work around a check.
    #[test]
    fn the_migration_warning_fires_only_with_credentials_and_no_table() {
        let armed = vars(&[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")]);

        // 1. credentials + no policy threaded at all → the warning.
        let msg = super::venue_arming_migration_message(&armed, None)
            .expect("credentials and no `[venues]` table is exactly the upgrade case");
        // 2. …and the same for a real, loaded policy that simply never named a venue.
        assert_eq!(
            super::venue_arming_migration_message(&armed, Some(&crate::MountPolicy::default())),
            Some(msg.clone()),
            "an undeclared policy is the same case as no policy"
        );

        // 3. SELF-SILENCING: an ALL-PAPER table produces the identical ceiling and silences it.
        let all_paper = crate::MountPolicy {
            venues: VenuePolicy::default().declare("bybit", VenueMode::Paper),
            ..crate::MountPolicy::default()
        };
        assert_eq!(
            all_paper.venue_mode("bybit"),
            VenueMode::Paper,
            "the fixture must leave the CEILING unchanged, or it is silencing by widening"
        );
        assert_eq!(
            super::venue_arming_migration_message(&armed, Some(&all_paper)),
            None,
            "a stated all-paper arming is a decision; warning at it trains people to ignore it"
        );

        // 4. No credentials → nothing was refused and there is nothing to paste.
        assert_eq!(super::venue_arming_migration_message(&HashMap::new(), None), None);
    }

    /// The message must be ACTIONABLE and must leak nothing: the file, the key, the venues that
    /// were refused, a paste-ready `[venues]` block — and never a credential key NAME, let alone a
    /// value. Same rule as `vike_config::refuse_credential_file_arming`, whose refusal deliberately
    /// echoes no value from the credential store.
    #[test]
    fn the_migration_warning_names_the_file_the_key_and_only_venue_slugs() {
        let armed = vars(&[
            ("BYBIT_DEMO_API_KEY", "super-secret-key"),
            ("BYBIT_DEMO_API_SECRET", "super-secret-secret"),
            ("OANDA_DEMO_API_KEY", "another-secret"),
            ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        let msg = super::venue_arming_migration_message(&armed, None).expect("the upgrade case");

        assert!(msg.contains("settings/policy.toml"), "names the FILE: {msg}");
        assert!(msg.contains("[venues]"), "names the KEY, as a pasteable table header: {msg}");
        assert!(msg.contains("bybit = \"live\""), "the block is paste-ready: {msg}");
        assert!(msg.contains("oanda = \"live\""), "…for EVERY refused venue: {msg}");
        assert!(!msg.contains("binance"), "a venue with no credentials is not in the list: {msg}");
        assert!(msg.contains("paper"), "says what the box is doing right now: {msg}");

        for (key, value) in armed.iter() {
            assert!(!msg.contains(key.as_str()), "leaked a credential key NAME ({key}): {msg}");
            assert!(!msg.contains(value.as_str()), "leaked a credential VALUE: {msg}");
        }
    }

    // ===========================================================================================
    // THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) — the Data Manager's Venues
    // tab reads its Effective column from here, so these gate that the column cannot disagree with
    // the mount.
    //
    // ⚠ The agreement is asserted against `make_engine_with_legs` ITSELF wherever a network-free
    // observable exists for it, not against a restatement of the rows: the tier a mount reaches is
    // visible offline only as PAPER-vs-not (the pre-connect budget refusal fires from
    // `would_mount_live_under`), so that half is driven through the real mount, and the
    // Live-vs-Demo half is driven against the SAME per-venue tier resolvers the arms call.
    // ===========================================================================================

    /// **THE column's gate: for every roster venue, at every ceiling, over the real credential
    /// fixtures, the projection's PAPER-vs-not verdict is what the mount actually does.**
    ///
    /// ⚠ **What this proves, stated exactly.** The agreement is BY CONSTRUCTION — `make_engine`
    /// consults `would_mount_live_under`, which is now `venue_arming_under` projected onto
    /// paper-vs-not — so this test cannot catch the two functions disagreeing (they are one
    /// function). What it DOES catch is the construction coming apart end-to-end: the mount
    /// consulting a different ceiling than the one it was handed, the seam moving back below the
    /// credential read, or the projection answering above its own ceiling. It drives the REAL
    /// `make_engine_with_legs` with a REAL `MountPolicy` and reads its only offline observable, the
    /// pre-connect budget refusal. The INDEPENDENT half — is the projected TIER the tier the arm
    /// would dial — is
    /// [`the_mainnet_switch_is_a_conjunct_of_the_ceiling_and_the_row_says_which_half_is_missing`],
    /// which compares against `cex_mainnet_enabled` rather than against a restatement.
    ///
    /// Anti-vacuity is built in two ways: the armed maps are asserted to arm SOMETHING at the live
    /// ceiling before the comparison runs, and every venue is driven with an EMPTY map too, so a
    /// projection that had degenerated into "always paper" would still have to agree with a mount
    /// that has not.
    #[test]
    fn the_arming_projection_agrees_with_the_real_mount_for_every_roster_venue() {
        use vike_config::VenueMode as Mode;

        let armed_for: std::collections::HashMap<&str, HashMap<String, String>> =
            live_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
        // ANTI-VACUITY: at least one fixture really does arm, or every agreement below is between
        // two functions that both always say paper.
        assert!(
            armed_for
                .iter()
                .any(|(v, map)| super::venue_arming_under(v, map, Mode::Live).0 != Mode::Paper),
            "no fixture arms anything — this test would prove nothing"
        );

        // ⚠ The THIRD map per venue, and it is the one that makes this test able to fail. Every
        // fixture in `live_arming_cases` arms at the DEMO tier, so under a `demo` ceiling the
        // capped and the UNCAPPED probes agree on all of them — a mount that consulted
        // `would_mount_live` instead of `would_mount_live_under` at its seam would sail through the
        // whole matrix. `mainnet_arming_cases` arms at the LIVE tier, where the two answers
        // diverge, and that is exactly the divergence the ceiling exists to create.
        let mainnet_for: std::collections::HashMap<&str, HashMap<String, String>> =
            mainnet_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
        let empty = HashMap::new();
        for venue in vike_model::VENUES {
            for map in [
                armed_for.get(venue).unwrap_or(&empty),
                mainnet_for.get(venue).unwrap_or(&empty),
                &empty,
            ] {
                for cap in Mode::ALL {
                    let (effective, block) = super::venue_arming_under(venue, map, cap);

                    // 1. A ceiling can only ever REFUSE. The screen renders `effective` beside
                    //    `ceiling`, so a projection that promoted would render a lie.
                    assert!(
                        effective <= cap,
                        "{venue} @ {cap}: projected {effective}, ABOVE the ceiling"
                    );
                    // 2. `block` and `effective` cannot contradict, in ONE direction: a CAPPED row
                    //    must carry a reason, and a CLEAR block must be at its ceiling.
                    //
                    //    ⚠ Deliberately not an `assert_eq!` of the two. The converse is false and
                    //    correctly so: a `paper` ceiling reports `Disarmed` and a thin build
                    //    reports `NoMountInThisBuild` — both at their ceiling, both with something
                    //    to say. A block is "what is holding this row where it is", which is
                    //    information even when nothing is being refused.
                    if effective < cap {
                        assert!(
                            !block.is_clear(),
                            "{venue} @ {cap}: capped to {effective} with NO reason — the Effective \
                             column would render a demotion the operator cannot explain"
                        );
                    }
                    if block.is_clear() {
                        assert_eq!(
                            effective, cap,
                            "{venue} @ {cap}: a clear block must mean the row is at its ceiling"
                        );
                    }
                    // 3. THE agreement, through the REAL mount.
                    let policy = ceiling(venue, cap);
                    let (out, live_set) = mount(venue, map, Some(&policy));
                    let mount_is_live = match out {
                        Err(crate::MountError::MissingRiskBudget { venue: named, .. }) => {
                            assert_eq!(named.as_str(), *venue, "the refusal must name the venue");
                            true
                        }
                        // ⚠ No catch-all `Err` arm: `MountError` has exactly one variant today, so
                        // one would be an unreachable pattern (`-D warnings`). A NEW variant makes
                        // this match non-exhaustive, which is a compile error naming this site —
                        // the right way round, since a new failure mode needs a decision here.
                        Ok(_) => {
                            assert!(
                                live_set.is_empty(),
                                "{venue} @ {cap}: a mount that armed live must have hit the \
                                 pre-connect budget refusal first"
                            );
                            false
                        }
                    };
                    assert_eq!(
                        effective != Mode::Paper,
                        mount_is_live,
                        "{venue} @ {cap}: the screen would say `{effective}` ({block:?}) while the \
                         mount {} — the Effective column exists precisely so these cannot differ",
                        if mount_is_live { "reaches its live arm" } else { "stays paper" }
                    );
                }
            }
        }
    }

    /// The credential shapes that arm a venue at its **LIVE** tier — [`live_arming_cases`]'s
    /// dangerous twin, and the only maps on which a `demo` ceiling and no ceiling at all give
    /// DIFFERENT answers.
    ///
    /// Only the four venues whose arm can reach live from a map: the three switched CEX ones
    /// (`{VENUE}_MAINNET=1` plus a LIVE key set) and aster, which is switchless and picks its tier
    /// from WHICH key set exists. Every other roster venue's arm hardcodes its demo endpoint, so
    /// there is no live-tier map to write for it.
    fn mainnet_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])]
    {
        &[
            (
                "binance",
                &[
                    ("BINANCE_MAINNET", "1"),
                    ("BINANCE_LIVE_API_KEY", "k"),
                    ("BINANCE_LIVE_API_SECRET", "s"),
                ],
            ),
            (
                "bybit",
                &[
                    ("BYBIT_MAINNET", "1"),
                    ("BYBIT_LIVE_API_KEY", "k"),
                    ("BYBIT_LIVE_API_SECRET", "s"),
                ],
            ),
            (
                "okx",
                &[
                    ("OKX_MAINNET", "1"),
                    ("OKX_LIVE_API_KEY", "k"),
                    ("OKX_LIVE_API_SECRET", "s"),
                    ("OKX_LIVE_API_PASSPHRASE", "p"),
                ],
            ),
            ("aster", &[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]),
        ]
    }

    /// The boolean probe is the projection's own answer, not a parallel implementation — asserted
    /// over the same matrix, because the pre-connect budget refusal rides on it and a divergence
    /// there is a live-money defect rather than a cosmetic one.
    #[test]
    fn the_live_intent_probe_is_exactly_the_projection_above_paper() {
        use vike_config::VenueMode as Mode;
        let empty = HashMap::new();
        for (venue, kv) in live_arming_cases() {
            let armed = vars(kv);
            for map in [&armed, &empty] {
                for cap in Mode::ALL {
                    assert_eq!(
                        super::would_mount_live_under(venue, map, cap),
                        super::venue_arming_under(venue, map, cap).0 != Mode::Paper,
                        "{venue} @ {cap}"
                    );
                }
            }
        }
    }

    /// **The Live-vs-Demo half**, which the paper-vs-not agreement above cannot see: the CEX
    /// venues reach the LIVE tier only with `{VENUE}_MAINNET=1` AND a `live` ceiling, and the row
    /// says which of the two is missing.
    ///
    /// ⚠ Driven against the SAME resolver the arms call (`cex_mainnet_enabled`), which is the
    /// strongest statement available with no network: the endpoint a mount dials is not observable
    /// offline, but the function that chooses it is the one being compared.
    #[test]
    fn the_mainnet_switch_is_a_conjunct_of_the_ceiling_and_the_row_says_which_half_is_missing() {
        use vike_config::ArmingBlock as Block;
        use vike_config::VenueMode as Mode;

        /// One switched-CEX case: the venue, its `{VENUE}_MAINNET` variable, its LIVE key pair and
        /// its DEMO key pair.
        struct SwitchCase {
            venue: &'static str,
            flag: &'static str,
            live_keys: [(&'static str, &'static str); 2],
            demo_keys: [(&'static str, &'static str); 2],
        }
        let cases: &[SwitchCase] = &[
            SwitchCase {
                venue: "binance",
                flag: "BINANCE_MAINNET",
                live_keys: [("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")],
                demo_keys: [("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")],
            },
            SwitchCase {
                venue: "bybit",
                flag: "BYBIT_MAINNET",
                live_keys: [("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")],
                demo_keys: [("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")],
            },
        ];
        for SwitchCase { venue, flag, live_keys, demo_keys } in cases {
            let mut both: Vec<(&str, &str)> = live_keys.to_vec();
            both.push((flag, "1"));
            let armed = vars(&both);

            // flag + live ceiling ⇒ the LIVE tier, nothing refused.
            assert_eq!(
                super::venue_arming_under(venue, &armed, Mode::Live),
                (Mode::Live, Block::None),
                "{venue}: the mainnet flag under a live ceiling must reach live"
            );
            // …and the mount's OWN resolution agrees about the flag.
            assert!(super::cex_mainnet_enabled(venue, &armed));

            // The same flag under a DEMO ceiling selects nothing — and with no demo keys in this
            // map the venue falls to paper, exactly as `make_engine` does (a mainnet host is never
            // signed with demo keys).
            assert_eq!(
                super::venue_arming_under(venue, &armed, Mode::Demo),
                (Mode::Paper, Block::NoCredentials),
                "{venue}: a demo ceiling deletes the mainnet tier, and no demo keys exist here"
            );

            // Demo keys, live ceiling, NO flag ⇒ demo, and the row NAMES the flag.
            let demo_only = vars(demo_keys);
            let (tier, block) = super::venue_arming_under(venue, &demo_only, Mode::Live);
            assert_eq!((tier, block), (Mode::Demo, Block::MainnetSwitchUnset), "{venue}");
            let row = vike_config::VenueArming {
                venue: vike_config::roster_id(venue).expect("a roster venue"),
                label: AccountLabel::Default,
                ceiling: Mode::Live,
                effective: tier,
                block,
            };
            assert!(row.why().contains(flag), "the row must NAME the flag: {}", row.why());
            assert!(row.is_capped());

            // …and at a DEMO ceiling the same map is at its ceiling: nothing is being refused, so
            // the row must NOT nag about a flag the operator did not ask to use.
            assert_eq!(
                super::venue_arming_under(venue, &demo_only, Mode::Demo),
                (Mode::Demo, Block::None),
                "{venue}: demo credentials under a demo ceiling are exactly what was asked for"
            );
        }

        // deribit is switchless — no `DERIBIT_MAINNET` exists — so a live ceiling over it still
        // reaches demo, and the row must say THAT rather than blaming a flag nobody can set.
        let deribit = vars(&[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]);
        assert_eq!(
            super::venue_arming_under("deribit", &deribit, Mode::Live),
            (Mode::Demo, Block::DemoOnlyArm),
        );
        assert!(vike_bridge_core::mainnet::mainnet_switch_for("deribit").is_none());
    }

    /// A venue this BUILD cannot mount reports [`vike_config::ArmingBlock::FeatureAbsent`] rather
    /// than "no credentials" — the row renders the disagreement between a portable `policy.toml`
    /// and a binary's compiled features instead of hiding it behind a credential complaint.
    ///
    /// ⚠ Written as two-armed `cfg`s rather than single assertions, so it states BOTH builds'
    /// contracts and neither can rot: no CI lane compiles every feature at once.
    #[test]
    fn a_venue_this_build_cannot_mount_says_so_instead_of_blaming_credentials() {
        #[allow(unused_imports)]
        use vike_config::ArmingBlock as Block;
        use vike_config::VenueMode as Mode;
        let empty = HashMap::new();

        #[cfg(not(feature = "polymarket"))]
        assert_eq!(
            super::venue_arming_under("polymarket", &empty, Mode::Live),
            (Mode::Paper, Block::FeatureAbsent),
        );
        #[cfg(feature = "polymarket")]
        {
            assert_eq!(
                super::venue_arming_under("polymarket", &empty, Mode::Live).1,
                Block::ExecFlagUnset,
                "with the feature ON, an empty map's first missing conjunct is POLY_EXEC"
            );
            assert_eq!(
                super::venue_arming_under("polymarket", &empty, Mode::Demo),
                (Mode::Paper, Block::LiveOnlyArm),
                "polymarket runs no testnet, so a demo ceiling leaves it on paper"
            );
        }

        #[cfg(not(feature = "ibkr"))]
        assert_eq!(
            super::venue_arming_under("ibkr", &empty, Mode::Live),
            (Mode::Paper, Block::FeatureAbsent),
        );
        #[cfg(not(feature = "fxcm"))]
        assert_eq!(
            super::venue_arming_under("fxcm", &empty, Mode::Live),
            (Mode::Paper, Block::FeatureAbsent),
        );
        // ⚠ The SDK-linked half is unreachable on every CI runner (none links ForexConnect), so
        // this asserts the refusal branch by name rather than pretending to cover both.
        #[cfg(feature = "fxcm")]
        assert_eq!(
            super::venue_arming_under("fxcm", &empty, Mode::Live),
            (
                Mode::Paper,
                if vike_fxcm::sdk_linked() { Block::NoCredentials } else { Block::SdkAbsent }
            ),
        );

        // dukascopy has no live arm in ANY build — a different fact from a missing feature, and
        // the row keeps them apart.
        assert_eq!(
            super::venue_arming_under("dukascopy", &empty, Mode::Live),
            (Mode::Paper, Block::NoLiveArm),
        );
    }

    /// [`super::venue_arming`] is the projection over the WHOLE roster: one row per venue, each
    /// carrying the ceiling it was asked about and the answer the per-venue call gives.
    ///
    /// ⚠ **With an empty credential map and no `[accounts]` table there is exactly one row per
    /// roster venue — the DEFAULT account's** — which is the table this projection returned before
    /// it knew about accounts at all. That equality is the whole "a box with no accounts behaves
    /// exactly as today" claim, asserted here at the projection rather than argued in prose.
    #[test]
    fn the_roster_projection_covers_every_venue_and_carries_its_ceiling() {
        use vike_config::VenueMode as Mode;
        let policy =
            VenuePolicy::default().declare("bybit", Mode::Live).declare("binance", Mode::Demo);
        let rows = super::venue_arming(&HashMap::new(), &policy);

        assert_eq!(rows.len(), vike_model::VENUES.len(), "one row per roster venue");
        let seen: Vec<&str> = rows.iter().map(|r| r.venue).collect();
        for venue in vike_model::VENUES {
            assert!(seen.contains(venue), "{venue} has no row");
        }
        for row in &rows {
            assert!(
                row.is_default_account(),
                "{}: an empty store names no second account",
                row.venue
            );
        }
        let by = |v: &str| rows.iter().find(|r| r.venue == v).expect("row").clone();
        assert_eq!(by("bybit").ceiling, Mode::Live);
        assert_eq!(by("binance").ceiling, Mode::Demo);
        assert_eq!(by("okx").ceiling, Mode::Paper, "an unnamed venue keeps the safe default");
        // …and every row agrees with the per-venue call it is built from.
        for row in &rows {
            assert_eq!(
                (row.effective, row.block),
                super::venue_arming_under(row.venue, &HashMap::new(), row.ceiling),
            );
        }
    }

    /// **A LABELLED account in the credential store gets a row of its own** — and, with no
    /// `[accounts]` line naming it, that row is PAPER with the block that says which line to write.
    #[test]
    fn a_labelled_account_in_the_store_gets_its_own_unarmed_row() {
        use vike_config::{ArmingBlock as Block, VenueMode as Mode};
        let policy = VenuePolicy::default().declare("bybit", Mode::Live);
        let vars = bybit_store_with_alt();

        let rows = super::venue_account_arming("bybit", &vars, Some(&policy));
        assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
        assert!(rows[0].is_default_account(), "the default account sorts FIRST");
        assert_ne!(rows[0].effective, Mode::Paper, "its credentials arm it, exactly as before");

        let alt = &rows[1];
        assert_eq!(alt.label.text(), Some("ALT"));
        assert_eq!(alt.effective, Mode::Paper, "a labelled account is not armed by the venue line");
        assert_eq!(alt.block, Block::AccountNotNamed);
        assert_eq!(alt.key(), "policy.accounts.bybit.ALT", "the row names the line to write");
        assert_eq!(alt.route_key(), "bybit#ALT");
        assert_eq!(rows[0].route_key(), "bybit", "the default account's routing does not move");
    }

    // ⚠ **THE HEADLINE — two armed accounts of one venue, and the SYMBOL reaching no arming
    // decision — is asserted in `crates/vike-mount/tests/shared_book_report.rs`**, not here, and
    // the reason is a gate rather than a preference: `crates/vike-ops/tests/settings_registry.rs`
    // HARVESTS STRING LITERALS out of `src/` to find undeclared environment reads, and a fixture
    // planting `HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT` here reads to that scanner as this library
    // reading a variable nothing declares. A `tests/` file is outside its scan by construction, so
    // a credential-shaped fixture belongs there — which is also where the rule's own unit tests
    // live (`crates/vike-config/tests/venue_accounts_table.rs`).
    //
    // The test this replaced was `two_armed_accounts_on_one_symbol_refuse_the_labelled_one`, and it
    // asserted the opposite of what is true: long BTC on the default account and short BTC on `ALT`
    // is an ordinary spread, and the two accounts are two wallets holding two positions.
}

#[cfg(test)]
mod account_event_lane_tests {
    //! [`super::account_event_sender`] — the seam that puts an account's identity on the ONE
    //! venue-tagged payload that carries no symbol.
    //!
    //! What it gates is the pairing: `account_route_key` renders the venue id itself for the
    //! DEFAULT account, and `vike_exec::EventSender::routed` stamps nothing when the key equals the
    //! payload's venue — so a single-account box emits `route_key: None` and its journal bytes are
    //! unchanged, which is the property that lets `make_engine_for_account` call this
    //! unconditionally instead of branching on which account it is mounting.
    //!
    //! Driven through the REAL `EventSender` and its ingest channel rather than by inspecting a
    //! field, because "does the payload change" is the question, not "is a field set".

    use super::{account_event_sender, account_route_key};
    use vike_model::account_keys::AccountLabel;
    use vike_model::events::{AccountState, Event};

    const VENUE: &str = "binance";

    /// What the lane actually delivered, given the account label the mount would have resolved.
    fn delivered(label: &AccountLabel) -> AccountState {
        let (plain, mut rx) = vike_exec::lanes::event_channel(4);
        let scoped = account_event_sender(&plain, &account_route_key(VENUE, label));
        scoped
            .blocking_send(Event::AccountState(AccountState {
                venue: VENUE.into(),
                balances: vec![("USDT".to_string(), 100.0)],
                ts: 1,
                route_key: None,
            }))
            .expect("receiver alive");
        match rx.blocking_recv() {
            Some(vike_exec::Ingest::Event(Event::AccountState(a))) => a,
            other => panic!("expected an AccountState ingest, got {other:?}"),
        }
    }

    /// THE INERTNESS HALF — and the one every existing deployment is in.
    #[test]
    fn the_default_accounts_lane_leaves_the_payload_byte_identical() {
        let got = delivered(&AccountLabel::Default);
        assert_eq!(
            got.route_key, None,
            "a box with no `[accounts]` table must emit the payload it always emitted"
        );
        assert_eq!(
            serde_json::to_string(&got).expect("serialize"),
            r#"{"venue":"binance","balances":[["USDT",100.0]],"ts":1}"#,
            "…and that means no `route_key` key on the wire at all"
        );
    }

    /// THE WORKING HALF: a labelled account's lane stamps the key `vike_core`'s router folds on,
    /// and it is the SAME string the engine's own `route_key` was set to at the mount.
    #[test]
    fn a_labelled_accounts_lane_stamps_the_engines_own_route_key() {
        let label = AccountLabel::parse("ALT").expect("a valid label");
        let expected = account_route_key(VENUE, &label);
        assert_ne!(expected, VENUE, "precondition: a labelled account decorates its key");
        assert_eq!(delivered(&label).route_key, Some(expected.as_str().into()));
    }
}
