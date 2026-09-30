//! vike-mount — the venue → `ExecutionClient`/`ReconClient` composition root, extracted from
//! `vike-app`'s `main.rs` as a pure behavior-preserving move so its live-exec wiring finally sits in
//! a CI-gated library (vike-app was compile-checked only — the wgpu build weight kept it out of the
//! test/clippy lane; `vike-desktop`, as it is now, mounts no venue at all).
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
//! ⚠ The ceiling can only ever REFUSE (`vike_config::VenueMode::cap` is `min`, never `max`) — except
//! for binance/bybit/okx and hyperliquid, where decision 0095 makes it the WHOLE answer:
//! `venues.bybit = "live"` chooses MAINNET outright, and there is no separate switch left to
//! decline. It binds at four places beyond the top-of-function refusal, and each is a venue whose
//! arm picks its own tier: the three CEX venues (`ceiling_selects_mainnet`), hyperliquid
//! (`vike_hyperliquid::config::Env::for_ceiling`, the same ceiling), aster's Live-first chain (which
//! reads no ceiling flag, and is the measured hole this stage closes), and polymarket's exec, whose
//! only tier is real money on Polygon.
//!
//! Layout (ReconFactory seam, wave-2 task 6): every venue's `ReconClient` is now built by that
//! venue's OWN `pub fn recon_client(...)` factory, living in its bridge crate next to the
//! `ReconClient` impl it constructs — this crate no longer holds any signer/transport/URL wiring
//! detail. [`recon::build_recon_client`] is a thin bybit/okx/binance dispatch (kept for its existing
//! callers/signature); the Hyperliquid bespoke-signer live+recon builder moved INTO that bridge on
//! decision 0088's B3 — `vike_hyperliquid::mount::live_mount_for_account` (calls
//! `vike_hyperliquid::recon_client` once its `instruments` fetch resolves `product` — see that fn's
//! doc for why the factory can't resolve `product` itself without a second network round-trip) —
//! and this crate's `("hyperliquid", _)` arm now only calls it and relays the result, the same
//! shape Polymarket's `live_mount_for_account` already has; the per-venue fallback
//! `SymbolProperties` grids are in [`fallback`] (Hyperliquid's own moved with its live-client
//! helper, into that same bridge module — [`fallback`]'s doc already carved this venue out).
//! Deribit, Aster, cTrader, Alpaca, IG, OANDA (and, behind the `ibkr` /
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
//! `recon_enabled` (the caller's already-resolved `vike_tradehub::reconcile_config::reconcile_gate`
//! verdict — ON by default for a live mount since S2) through
//! `vike_bridge_core::venue_mount::recon_if_enabled`: reconcile off ⇒ the factory is never called
//! and the venue does no authenticated network work at mount. That gate is an explicit PARAMETER,
//! deliberately NOT
//! `recon_trigger.is_some()` — the trigger is a per-venue RECONNECT poke wired for only four venues,
//! and `vike_run::build_node` passes `None` for these five even with reconciliation fully on, so
//! inferring the global gate from it would silently stop reconciling them.
//!
//! Polymarket is the ONE venue with **no testnet**, so its arm is gated TWICE where every other
//! venue is gated once: `flags.poly_exec` mounts the real (REAL-MONEY, Polygon-mainnet)
//! `ExecutionClient` — whose exec thread also owns the authenticated user-WS fill pump, the only
//! lane a Polymarket fill/cancel ever arrives on — and `flags.poly_reconcile`, on top of the master
//! gate, mounts the `ReconClient` (`poly_recon_wanted`; ⚠ that arm read the venue flag ALONE
//! until 2026-09-06, which built a handle a driver-less root then dropped, and since S2 the master
//! gate is ON BY DEFAULT for a live mount — so `flags.poly_reconcile` is now the ONE act this venue
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
//! root is likewise passed in by the caller (`tick_store_root`) — `vike-app` owned the READS that
//! resolution needs (the variable, the boot's settings directory, its own executable path) because
//! it used the same root elsewhere, until its `tick_store_root` went with the local core; the
//! LADDER itself is
//! `vike_model::tick_store_path::resolve_tick_store_root`. (It said "shared with the daemon's
//! twin" — there is no twin: that copy went with `record-feeds`, and the resolver now has no
//! production caller at all. See its own module doc.)
//! ⚠ **Decision 0095: no `{VENUE}_MAINNET` variable is read anywhere in this tree any more** — the
//! four switches (three CEX plus hyperliquid) are DELETED, and the ceiling alone chooses the
//! network. For the three CEX venues [`make_engine`] resolves [`arming::ceiling_selects_mainnet`]
//! EXACTLY ONCE per mount and threads the resulting `bool` into the grid pre-fetch, the exec spawn
//! and [`recon::build_recon_client`]; hyperliquid resolves
//! `vike_hyperliquid::config::Env::for_ceiling` the same way. No spawned adapter thread reads
//! anything, so a mount can never sign mainnet credentials against demo hosts.
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
// (`fxcm_credentials_without_a_loadable_shim_refuse_the_live_mount` and
// `fxcm_probes_live_only_with_credentials_and_a_linked_sdk`), which reach it via `super::`. Gated on
// the feature alone, an `--features fxcm` LIB build imports a name it never uses and `-D warnings`
// refuses it — which is exactly how the fxcm lane found this.
#[cfg(all(test, feature = "fxcm"))]
use arming::fxcm_live_intent;
use arming::{
    account_arming_under, account_ceiling, account_event_sender, account_route_key,
    arm_universal_defaults, binance_withdraw_gate, ceiling_permits_live, ceiling_selects_mainnet,
    cex_cred_choice, margin_mode_grid, multiplier_grid, report_unaddressable_accounts,
    require_live_risk_budget, venue_ceiling, would_mount_live_under_policy,
};
use paper_fallback::{
    paper_client, paper_engine, report_capped_to_paper, report_halt_admit, report_halt_admit_armed,
    venue_arming_migration,
};

mod arming;
pub mod book_identity;
mod contract;
mod dukascopy;
mod error;
mod exclusive;
mod fallback;
pub mod hyperliquid;
mod paper_fallback;
pub mod policy;
pub mod preflight;
mod recon;
mod registry;
pub mod server_time;
pub mod startup;
pub mod symbol_grid;
pub mod transition;

pub use arming::{
    known_accounts, resolve_fee_schedule, shared_book_ceiling_note, shared_books_for,
    symbol_for_account, unaddressable_accounts_message, unaddressable_accounts_text,
    venue_account_arming, venue_arming, would_mount_live, would_mount_live_under,
};
pub use policy::MountPolicy;
pub use recon::build_recon_client;
// Crate-root vocabulary: `registry` is private, so this is the only name for the row type and its
// lookup (docs/decisions/0096).
pub use registry::{VenueRow, row_of};
pub use symbol_grid::declared_grid_source;
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

/// **Polymarket's reconcile decision, extracted so it can be tested** — `recon_enabled` (the
/// caller's already-resolved `vike_tradehub::reconcile_config::reconcile_gate` verdict) AND the venue's
/// own `flags.poly_reconcile`.
///
/// ⚠ **It reads the master gate, and until 2026-09-06 it did not.** The `("polymarket", _)` arm
/// keyed on `poly_reconcile_enabled` ALONE, and this was defended everywhere in the tree as "the
/// venue has its own equivalent inner gate". It was never equivalent: the client this arm builds is
/// only ever USED by the driver both roots mount under the master gate, so the effective condition
/// was already `flags.poly_reconcile is on AND recon_enabled` — the arm simply performed the venue's
/// authenticated L1→EOA + `/auth/derive-api-key` round trip first and let the handle be dropped
/// when the gate said no. That is the exact build-then-discard defect
/// [`vike_bridge_core::venue_mount::recon_if_enabled`] exists to have removed for the other five
/// inline venues, wearing a different name.
///
/// ⚠ **And S2 is what made the difference visible, in the direction an operator feels.** The master
/// gate is now ON BY DEFAULT for a mount that arms a live venue account, so a box carrying
/// `flags.poly_reconcile` on and no `VIKE_RECONCILE` reconciles Polymarket where it used to build a client
/// and reconcile nothing — authenticated Polygon-MAINNET reads against a real-money venue with no
/// testnet. That is a real change and it is NAMED rather than smoothed over:
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the verdict and the
/// alternative the owner may prefer, and `docs/ops/reconcile-on-restart.md` tells the operator
/// which single line turns it back off (`flags.poly_reconcile` off, or `VIKE_RECONCILE_OFF=1`).
/// What has NOT changed is that Polymarket still needs an act nobody else needs: every other venue
/// reconciles on the default alone, this one needs `flags.poly_reconcile` on top of it.
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
    registry: &'static [VenueRow],
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
        registry,
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
    registry: &'static [VenueRow],
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
        registry,
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
    registry: &'static [VenueRow],
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
    // THE UNADDRESSABLE-ACCOUNT REPORT, once per process and BEFORE anything is mounted — the
    // accounts this box names on a venue whose arm addresses only one. It is emitted HERE, from the
    // fan-out, rather than from `make_engine_for_account` where every other arming line is said,
    // because such an account never reaches that function (`accounts_to_mount` drops it — that IS
    // the refusal) and its VENUE may never be mounted at all: dukascopy, the venue this was written
    // for, carries no `vike_run::WIRED_MARKETS` row, so a line emitted from its own mount would
    // be a line nobody ever reads. The message names every refused account at once and is
    // `Once`-latched, exactly like `venue_arming_migration` below.
    //
    // ⚠ Since 2026-09-15 it names NOTHING on any box: dukascopy was the last venue whose arm could
    // not address a second account, and `arming::arm_addresses_accounts` now carries every roster
    // id. The call stays because a venue scaffolded by `just new-venue` is deliberately NOT added
    // to that list, so the next venue to need this message is the next venue to exist.
    report_unaddressable_accounts(registry, vars, policy);
    // ⚠ The WHOLE policy, not its `venues` table: the projection resolves a dukascopy account out of
    // `MountPolicy::accounts` — the one snapshot this fan-out's own `make_engine_for_account` calls
    // resolve it from, so the rows below describe exactly what the loop underneath will mount.
    let rows = venue_account_arming(registry, venue, vars, policy);
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
    // ⚠ **CAPPED — the pair count is QUADRATIC and this loop is the only thing standing between an
    // operator and 1,225 startup lines.** `shared_books` pairs every account on a book with every
    // other, so N accounts of one wallet is N(N-1)/2 findings; at the fifty this design is scoped
    // for that is a flood in which the thing worth knowing — that fifty of them are ONE wallet —
    // appears in no individual line. The split is arithmetic and lives in `vike-config` where it
    // can be TESTED, which this emission site cannot be: two accounts can only both be ACTIVE on a
    // venue with a real live arm, so a test that reached this `warn!` would dial the venue on its
    // next statement (the blind spot declared above).
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
        // The aggregate line, and it says something none of the lines above can: how many DISTINCT
        // accounts sit on each book. `ceiling_note` rides here too, because at this scale it is the
        // multiplier that has grown worst — the per-engine ceiling is applied once per account on
        // the book, not once per book.
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
    if rows.is_empty() {
        // A venue `vike_model::VENUES` does not carry — a test id, a sim id. It has exactly one
        // account, nothing can name a second, and there is no policy row to consult, so the fan-out
        // is the single mount it has always been.
        let engine = make_engine_for_account(
            registry,
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
            registry,
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
///   [`vike_model::account_keys::AccountRef::route_key`] — the bare venue id for the default account, so
///   `vike_ops::live_lock`'s `LIVE-<route_key>.lock` sentinel does not move for any existing
///   deployment.
///
/// ⚠ It mounts whatever it is asked to mount. **The shared-BOOK report is NOT made here** — it is a
/// fact about the SET of accounts on a venue, which this function cannot see one account at a time.
/// [`make_engine_accounts`] is the fan-out that owns it, and it is the entry point every
/// composition root uses.
#[allow(clippy::too_many_arguments)]
pub fn make_engine_for_account(
    registry: &'static [VenueRow],
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
    // The halt-admit mode in force at this venue, with the DEGRADE reported once, here, at mount —
    // never at the first halted submit. Whether `verify` actually ARMED is a different question and
    // is deliberately NOT answered here: this line runs before credentials and before cTrader's
    // blocking handshake, either of which can land the venue on paper. `report_halt_admit_armed`
    // says that in `assemble_engine`, once the mount's outcome is known, and both docs carry the
    // reasoning.
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
    // `vike-tradehub` builds that from its own `vike_config::load`, as `vike-app` did while it
    // mounted), so the `None` arm
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
        venue_arming_migration(registry, vars, policy);
        report_capped_to_paper(registry, venue, vars, policy);
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
        let (mut engine, recon) =
            paper_engine(venue, symbol, account, declared_legs, risk_profile, policy);
        engine.route_key = route_key;
        return Ok((engine, recon));
    }
    // THE FOLD, spelled as the one method that spells it. `VenueMode::cap` is `min`, never `max`
    // (its doc explains why it exists as a named method rather than as a `.min()` at each site):
    // the highest tier the venue's own mechanisms could reach is LIVE, and the ceiling caps it. A
    // reviewer looking for a widening bug is looking for a `max`, and there is one place to look.
    let live_permitted = ceiling_permits_live(mode);
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
    // THE VENUE'S OWN HALF: a contract row mounts through its bridge's `VenueMount`, a feature-off
    // row is the paper client, and a `Legacy` row — every venue until its port lands — runs the
    // legacy arm unchanged. All three produce ONE shape, `MountParts`, so the shared tail below
    // cannot drift between them (docs/decisions/0096).
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
                recon_trigger,
                properties_rec,
                risk_profile,
                policy,
                mode,
                live_permitted,
                halt_admit,
                static_default,
            },
        )?,
        Some(VenueRow::FeatureAbsent { .. }) => {
            contract::absent_parts(venue, symbol, static_default)
        }
        Some(VenueRow::Legacy(_)) | None => legacy_parts(
            registry,
            venue,
            symbol,
            account,
            declared_legs,
            vars,
            live_events,
            recon_enabled,
            recon_trigger,
            properties_rec,
            risk_profile,
            policy,
            mode,
            live_permitted,
            halt_admit,
            static_default,
        )?,
    };
    assemble_engine(
        venue,
        symbol,
        account,
        declared_legs,
        parts,
        live_venues,
        route_key,
        halt_admit,
        risk_profile,
        policy,
    )
}

/// What a venue's mount produced, before the shared tail folds it — the one shape the legacy arms
/// and the contract path both produce, so the tail cannot drift between them.
pub(crate) struct MountParts {
    pub(crate) client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub(crate) recon: Option<Box<dyn vike_exec::recon::ReconClient>>,
    pub(crate) limits: vike_exec::RiskLimits,
    pub(crate) contract_size: f64,
    pub(crate) default_margin_mode: vike_model::MarginMode,
    pub(crate) symbol_grids: indexmap::IndexMap<String, vike_exec::SymbolGrid>,
    pub(crate) grid_source: vike_bridge_core::venue_mount::DeclaredGridSource,
    /// The tier `record_authenticated_account` addresses; `None` asks nothing.
    pub(crate) record_tier: Option<vike_config::VenueMode>,
    /// `(book, evidence, tier)` the venue itself named (hyperliquid's `userRole`).
    pub(crate) identity: Option<(String, &'static str, vike_config::VenueMode)>,
    pub(crate) live: bool,
    pub(crate) static_default: vike_model::FeeSchedule,
}

/// ⚠ TRANSITIONAL — **the legacy per-venue arms**, for every venue whose registry row is still
/// `VenueRow::Legacy`: the credential resolution, the pre-connect budget refusal and the fourteen-arm
/// `match (venue, creds)`, moved here unchanged out of [`make_engine_for_account`] so the contract
/// path could sit beside it. Each venue's port deletes its arm; the migration's final task deletes
/// this function (docs/decisions/0096).
///
/// It records nothing and builds no engine: it hands the shared tail ([`assemble_engine`]) the
/// client, the reconcile handle, the grid and whether the arm went LIVE, which is everything the
/// arms used to write into `live_venues` and the tail's locals directly.
#[allow(clippy::too_many_arguments)]
fn legacy_parts(
    registry: &'static [VenueRow],
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    vars: &HashMap<String, String>,
    live_events: &vike_exec::EventSender,
    recon_enabled: bool,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
    mode: vike_config::VenueMode,
    live_permitted: bool,
    halt_admit: vike_model::HaltAdmit,
    static_default: vike_model::FeeSchedule,
) -> Result<MountParts, MountError> {
    use vike_bridge_core::credentials::{Environment, load_credentials_for_account};
    // Go-live credentials (decision 0095): for binance/bybit/okx the ceiling ALONE chooses the
    // network (`ceiling_selects_mainnet`), so the credentials a mount signs with and the hosts the
    // adapter binds always flip together. A `live` ceiling ⇒ load the venue's LIVE (mainnet) key
    // set; `demo` and below — and EVERY non-CEX venue — ⇒ load DEMO exactly as before, so that path
    // is byte-identical. SAFETY (absent-credentials-is-the-live-gate): a `live` ceiling with NO
    // live creds resolves to `None` here and stays PAPER — it never signs a mainnet host with demo
    // keys. See `cex_cred_choice` for the pure ceiling×creds matrix + its test.
    //
    // This ONE resolution is the venue's single source of truth for the whole mount — it is
    // threaded below into the grid pre-fetch, the exec spawn and `build_recon_client`, so no
    // spawned thread re-reads anything for itself.
    // Decision 0095: for binance/bybit/okx the ceiling IS the network, so `mainnet` reduces to
    // whether this venue is one of the three AND the ceiling reached `live`. `venues.binance =
    // "demo"` therefore mounts the DEMO tier — and if no demo keys exist, `load_credentials_from`
    // answers `None` and the venue stays PAPER, because a mainnet host is never signed with demo
    // keys (`cex_cred_choice`'s `MainnetNoCreds`, which the `warn!` below still reports).
    let mainnet = ceiling_selects_mainnet(venue) && live_permitted;
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
            "{venue}: the ceiling is `live` (MAINNET) but no LIVE credentials are present → \
             staying PAPER (a mainnet host is never signed with demo keys; absent credentials are \
             the live gate)"
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
    // the question THIS mount will ask, and the ceiling changes it. Under `demo`, decision 0095
    // means a binance store holding only LIVE-tier keys and one holding only DEMO-tier keys
    // disagree with the ceiling-BLIND probe in BOTH directions — the LIVE-tier-only store makes
    // the uncapped probe true while the arm below (which resolves the DEMO ceiling) stays paper,
    // and the DEMO-tier-only store makes the uncapped probe false while the arm below genuinely
    // arms at the demo tier. The second of those is the one that matters: an armed venue with no
    // bounded budget is exactly what this refusal exists to stop.
    //
    // ⚠ …and per ACCOUNT, for the same reason it is ceiling-aware: the probe has to answer the
    // question THIS mount will ask. Reading the DEFAULT account's credentials for a labelled mount
    // disagrees in both directions — a box whose second account alone is configured would escape
    // the budget refusal on a mount that genuinely arms, which is the direction that matters. For
    // `AccountLabel::Default` this is `would_mount_live_under` unchanged.
    if account_arming_under(registry, venue, account, vars, mode, policy).0
        != vike_config::VenueMode::Paper
    {
        require_live_risk_budget(
            venue,
            &risk_profile.map(vike_exec::ProfileRisk::to_risk_limits).unwrap_or_default(),
            risk_profile.is_some(),
        )?;
    }
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
    // hyperliquid arms build their `ReconClient` inline (each needs a non-REST or bespoke-signer
    // handshake `build_recon_client` can't produce); the ctrader arm instead receives an
    // already-built one back from `vike_ctrader::mount::live_mount_for_account` (decision 0088 B5) —
    // its handshake is no different, only which crate opens the socket moved. Stays `None` for paper
    // venues. A side-settable local (rather than a second match-arm return value) so `client`'s
    // match keeps its original single-type shape instead of a second tuple-typed annotation.
    let mut recon: Option<Box<dyn vike_exec::recon::ReconClient>> = None;
    // Hyperliquid's own `userRole`-derived account identity (decision 0088's B3): set inside its
    // arm below and recorded once after the match, beside the CEX venues'
    // `record_authenticated_account` call. The bridge cannot record it itself — that write needs
    // `AccountDirectory`/`VenueMode`, types a bridge may never hold — so it hands back a
    // `vike_hyperliquid::mount::MasterOutcome` and this crate does the write. The `bool` alongside
    // it is the resolved network tier (`env == Env::Live`), carried here because the generic
    // top-level `mainnet` local a few lines up is the CEX-specific `ceiling_selects_mainnet(venue)
    // && live_permitted` conjunct (decision 0095) and is not this venue's answer.
    let mut hyperliquid_master: Option<(bool, vike_hyperliquid::mount::MasterOutcome)> = None;
    // Whether the arm below went LIVE — set where each arm used to record its route key in
    // `live_venues`; `assemble_engine` records it now, once, for every venue.
    let mut live = false;
    // API-KEY PERMISSION GATE (api-key-permissions capability map, STEP-2 — the wiring its STEP-1
    // reported as owed). Resolved BEFORE the match, not inside a match guard, so the one blocking
    // signed read it can issue is visible at the top level rather than hidden in a pattern. Only a
    // credentialed binance MAINNET mount probes anything (see `binance_withdraw_gate`); every other
    // (venue, creds) pair short-circuits to `Allow` with no network and no behavior change. A
    // `Refuse` makes the `("binance", Some(c))` arm below not match, so the venue falls through to
    // the paper `_` arm — never marked live, never spawned — exactly as absent credentials do.
    let binance_key_gate = match (venue, creds.as_ref()) {
        ("binance", Some(c)) => binance_withdraw_gate(mainnet, c, vars),
        _ => WithdrawGate::Allow,
    };
    let client: Box<dyn vike_exec::ExecutionClient + Send> = match (venue, creds) {
        ("bybit", Some(c)) => {
            live = true;
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
            // The `execution.fast` hint — `venue.bybit.fast_exec` (decision 0095), read once out of
            // the credential map under its folded name.
            let bybit_fast_exec = vike_bybit::user_data::fast_exec_from(vars);
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
                bybit_fast_exec,
            ))
        }
        ("okx", Some(c)) => {
            live = true;
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
            live = true;
            // #771 + go-live: decision 0095's ceiling-derived `mainnet` (`ceiling_selects_mainnet`
            // + the loaded creds `c`) becomes the effective `Environment` here — through the
            // adapter's own pure `resolve_env_from` core, so the demo→mainnet upgrade rule lives in
            // exactly one place — and is threaded into BOTH the grid pre-fetch and the exec client,
            // which bind the hosts those live creds authenticate against. STEP 2: the adapter's
            // exec thread never re-resolves it, so this `env` is authoritative end-to-end;
            // `mainnet == false` ⇒ `Demo`, byte-identical to before the ceiling took over this
            // decision.
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
            // The TRADE_LITE early-fill hint — `venue.binance.trade_lite_fill` (decision 0095),
            // read once out of the credential map under its folded name, like every other venue
            // fact here.
            let binance_trade_lite_fill = vike_binance::perp_user_data::trade_lite_fill_from(vars);
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
                binance_trade_lite_fill,
            ))
        }
        ("deribit", Some(c)) => {
            live = true;
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
            recon = vike_bridge_core::venue_mount::recon_if_enabled(recon_enabled, || {
                vike_deribit::recon_client(&c, symbol)
            });
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
            // `Environment::Live` FIRST and there is NO flag to refuse it — aster has never had a
            // `{VENUE}_MAINNET`-shaped switch, so `vike_config::CREDENTIAL_FILE_ARMING_REFUSED`
            // structurally cannot carry a row for it. An `ASTER_LIVE_*` pair sitting in
            // `secrets.env` was therefore, by itself, an authenticated MAINNET session on a daemon
            // that never asked for one.
            //
            // The ceiling is the refusal that did not exist: under anything below `live` the Live
            // attempt is DELETED from the chain — not attempted-and-discarded, not attempted with
            // the result ignored. Under `live` the chain is byte-identical to before.
            //
            // ⚠ `..._for_account`, not `..._from`: this venue is MAINNET in practice, so a labelled
            // account reading the DEFAULT account's `ASTER_LIVE_PRIVATE_KEY` would be a second live
            // client signing for the first account's real-money wallet. For `AccountLabel::Default`
            // the two calls read the same key names.
            //
            // The chain itself moved to `vike_aster::signing::mountable_tier_for_account`
            // (decision 0088, B1) — this arm is now the caller rather than the spelling, matching
            // the shape `vike_mount::arming`'s `account_arming_raw`, `vike_mount::server_time`'s
            // `aster_time` and `vike_tradehub::venue_arming`'s `cex_mainnet_enabled` all reach the
            // same function through.
            let resolved =
                vike_aster::signing::mountable_tier_for_account(account, vars, live_permitted);
            if let Some((env, c)) = resolved {
                live = true;
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
        // AND `flags.poly_exec`) where every other venue is gated once, and why the flag is checked
        // BEFORE the factory so an off one makes no network call at all.
        //
        // Two INDEPENDENT opt-ins, composing four ways:
        // - `flags.poly_exec`      → `live_mount_from_vars` spawns the real `PolymarketExecutionClient`,
        //                        whose exec thread ALSO owns the authenticated user-WS fill pump and
        //                        its A3 resync (the return lane: on this venue every post-acceptance
        //                        terminal — fill, cancel, expiry — arrives only there). When
        //                        reconcile is also on, that ONE call returns the `ReconClient` too,
        //                        over the SAME L2 handshake and the SAME `PolymarketRegistry`, so
        //                        order reports finally re-key to local coids instead of `None`.
        // - `flags.poly_reconcile` → reconcile. With exec OFF it is the pre-existing recon-only mount
        //                        (`recon_client_from_vars`, fresh empty registry, exec PAPER).
        // - neither            → nothing is built, no network call, venue stays PAPER.
        //
        // Both are settings rows (`flags.poly_exec`, `flags.poly_reconcile`) the daemon folds into
        // this map — decision 0095; no environment variable arms them. `live_mount_from_vars`
        // resolves the L1 signer address (NOT the funder — ClobAuth must name the key's own EOA) and
        // derives the L2 trio over ONE blocking, proxy-routed round-trip at mount, exactly like the
        // deribit/ig/oanda inline handshakes (and ctrader's, now inside its own bridge function);
        // ANY failure is `None` — the venue falls back to paper, never a mount failure or a panic.
        //
        // ACCOUNT-WIDE, like the recon half: orders, fills, positions and balance all key off the
        // wallet, not `symbol`. `symbol` is used for exactly one thing — a best-effort pre-resolution
        // of that token's NegRisk signing domain, inert on failure. No `RiskLimits` pre-fetch /
        // `contract_size` applies (Polymarket has no instrument grid of that shape; shares are whole
        // and price is a probability). Interval-only reconcile: `recon_trigger` is not read, and
        // polymarket is not in the app's `recon_feed_statuses`, so its health gate reads Healthy.
        //
        // POLICY: `flags.poly_reconcile` WITHOUT `flags.poly_exec` diffs live venue state against a PAPER
        // engine, and wants `VIKE_RECONCILE_POLICY=quarantine`. The reason is `PositionDrift` — the
        // one kind `hybrid` genuinely auto-applies — which would fold the LIVE account's position
        // into the paper engine's books at the venue's avg price. With exec mounted, both sides are
        // the same account and `hybrid` is sound.
        //
        // ⚠ This comment used to say `hybrid` "would auto-cancel every paper order each pass" via
        // `OrphanLocalOrder`. That was false — the kind resolves to zero events under every policy
        // and `resolve` synthesizes no cancel at all. See
        // `vike_polymarket::exec_plane::recon_client`'s `## ⚠ ROLLOUT` doc and
        // `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
        // See `vike_polymarket::poly_reconcile_enabled` and `vike_polymarket::exec_plane::mount` — the authorities.
        #[cfg(feature = "polymarket")]
        ("polymarket", _) => {
            // BOTH gates, and the master one is not decoration here: it is what stops this arm
            // doing the venue's authenticated L2 round trip to build a handle the driver will never
            // be mounted to use. See [`poly_recon_wanted`] for the defect that spelling removes and
            // for what S2 changed for an operator who set `flags.poly_reconcile` and nothing else.
            let want_recon =
                poly_recon_wanted(recon_enabled, vike_polymarket::poly_reconcile_enabled(vars));
            // ⚠ THE CEILING AND `flags.poly_exec` ARE BOTH REQUIRED, and neither replaces the other.
            // `venues.polymarket = "live"` does NOT arm exec — the venue's own double gate stands
            // and `flags.poly_exec` is still mandatory. What the ceiling adds is the refusal in the
            // other direction, which is the one that was missing: `paper` overrides `flags.poly_exec`
            // outright, and it does so from ABOVE this arm (the early return at the top of this
            // function), so `poly_exec_enabled` — which reads the map the daemon folds
            // `flags.poly_exec` into — is never even consulted. A
            // gate that ran after it could be argued with; one that runs before it cannot.
            //
            // `demo` is refused here rather than at that early return, because it is a
            // venue-SPECIFIC fact and this is the venue: Polymarket has NO testnet, so `demo` names
            // a tier that does not exist and the only tier the arm could honour is REAL MONEY on
            // Polygon mainnet. Arming it would be the ceiling widening a mount, which
            // `VenueMode::cap` exists to make impossible.
            //
            // ⚠ RESIDUAL, stated rather than implied: this gates EXEC. Under `demo` the RECON-ONLY
            // lane below still runs when `flags.poly_reconcile` is on, exactly as it does today — those are
            // authenticated mainnet READS, they place no order, and narrowing them is a separate
            // decision about what a ceiling governs. Under `paper` nothing in this arm runs at all.
            let exec_permitted = vike_polymarket::poly_exec_enabled(vars) && live_permitted;
            if vike_polymarket::poly_exec_enabled(vars) && !live_permitted {
                tracing::warn!(
                    venue,
                    ceiling = %mode,
                    "flags.poly_exec is on, but this deployment's arming ceiling for polymarket is \
                     not `live` → exec stays PAPER. Polymarket has no testnet: `demo` names a tier \
                     that does not exist here, so the ceiling refuses rather than arming the only \
                     tier there is (real money on Polygon mainnet). Run `vike-cli config set \
                     policy.venues.polymarket live` to allow it."
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
                    live = true;
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
                                "polymarket: flags.poly_reconcile without a live exec mount → \
                                 RECONCILE-ONLY (exec is PAPER). Run under \
                                 VIKE_RECONCILE_POLICY=quarantine: an auto-applying policy folds \
                                 PositionDrift, which would import the LIVE account's position \
                                 into the PAPER engine's books at the venue's avg price. The \
                                 startup line from vike_tradehub::reconcile_config names exactly what \
                                 the resolved policy auto-applies. See \
                                 vike_polymarket::poly_reconcile_enabled"
                            );
                        } else {
                            tracing::warn!(
                                "polymarket: flags.poly_reconcile is on but no reconcile client could be \
                                 built → reconcile-inert"
                            );
                        }
                    }
                    Box::new(paper_client(venue, symbol, static_default))
                }
            }
        }
        // Hyperliquid's credentials are the bespoke private-key shape (loaded inside the bridge's
        // own `vike_hyperliquid::mount::live_mount_for_account` via `config::load_for_account`, not
        // the standard `creds` above), so it matches on `_` creds and gates itself. `spot`/`perp`
        // both route here (one engine per venue); the perp `"BTC"` is mounted. That function also
        // builds HL's `ReconClient` (bespoke signer/transport path, not `build_recon_client`) and
        // returns it alongside the exec client — wired into `recon` here. ⚠ This arm used to call a
        // `vike-mount`-local helper of the same shape (`hyperliquid::hyperliquid_live_client`);
        // decision 0088's B3 moved that whole builder down into the bridge, and this arm is now
        // only the caller — see `crates/bridges/hyperliquid/src/mount.rs`'s module doc.
        //
        // THE POLICY CONSUMER (Phase 6c): this is the one venue on the roster with no native market
        // order, so it is the one arm `MountPolicy::market_slippage` binds. `None` (no
        // `policy.toml`, or a policy that does not name the key) is byte-identical — the bridge
        // resolves it through `vike_hyperliquid::exec::market_slippage_for`, whose unset path
        // returns the adapter's own historical literal.
        ("hyperliquid", _) => {
            // The arming ceiling, resolved to a network tier HERE — decision 0095: `live` is
            // mainnet, and this is the ONE thing this arm still does before handing off to the
            // bridge. The bridge reads no environment and is handed no ceiling — see
            // `crates/vike-mount/src/hyperliquid.rs`'s module doc.
            let env = vike_hyperliquid::config::Env::for_ceiling(live_permitted);
            let attempt = vike_hyperliquid::mount::live_mount_for_account(
                symbol,
                declared_legs,
                env,
                vars,
                account,
                live_events,
                &mut limits,
                &mut default_margin_mode,
                recon_trigger,
                policy.and_then(|p| p.market_slippage),
            );
            // The identity handshake: a real `userRole` probe when the key/signer resolved far
            // enough to ask, whether or not the mount goes on to succeed (see `LiveMountAttempt`'s
            // doc). Recorded once after the match, beside the CEX venues' equivalent call — this
            // arm only carries the answer forward.
            hyperliquid_master =
                attempt.master.map(|master| (env == vike_hyperliquid::config::Env::Live, master));
            match attempt.live {
                Some(mounted) => {
                    // The DECLARED LEGS' own grids, out of the SAME instrument fetch the bridge
                    // already performed — see `LiveMount::declared_leg_properties`'s doc for why
                    // the bridge cannot fold this map itself.
                    symbol_grids =
                        symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
                            mounted.declared_leg_properties.get(leg).copied()
                        });
                    live = true;
                    recon = Some(mounted.recon);
                    mounted.client
                }
                None => Box::new(paper_client(venue, symbol, static_default)),
            }
        }
        // cTrader uses OAuth token creds (CTRADER_CLIENT_ID/_SECRET + CTRADER_DEMO_ACCESS_TOKEN/
        // _REFRESH_TOKEN), NOT the generic `{VENUE}_DEMO_API_KEY` shape, so `load_credentials_from`
        // above returns `None` for it — it self-gates inside `vike_ctrader::mount::
        // live_mount_for_account` (like the aster/hyperliquid arms match on `_`). This is the
        // EXEC-ONLY mount: market data comes from the separate `CtraderData` feed the daemon wires
        // elsewhere (`crates/vike-tradehub/src/feeds.rs`'s `wire_venue_feeds`; `vike-app` wired
        // one while the desktop mounted venues).
        //
        // ⚠ decision 0088 B5: the OAuth credential load, the blocking protobuf/TLS handshake, the
        // dedicated recon socket and the halt-admit wiring used to live inline, right here — they
        // now live in `vike_ctrader::mount::live_mount_for_account`, the bridge's own live-mount
        // seam (the same cut polymarket's `live_mount_for_account` and oanda's
        // `mountable_tier_for_account` already made). This arm keeps only what is generic across
        // every venue: folding the returned client/recon pair into the shared locals below. The
        // halt-admit REPORT is written once for every venue, not cTrader's to own — in
        // `assemble_engine`, since the venue mount contract split the tail out of this function.
        //
        // ⚠ WEAKER ROBUSTNESS CONTRACT (fast-follow, called out in the PR body): unlike every other
        // live arm, cTrader's `connect_and_auth_exec` is a BLOCKING, FALLIBLE protobuf/TLS handshake
        // performed synchronously at mount time — the crypto/deribit/aster arms instead spawn an
        // actor that reconnects internally and never fails the mount. A connect failure therefore
        // DEMOTES cTrader to PAPER for the whole session (there is no in-thread reconnect). A
        // follow-up should move the connect into a self-healing spawn like the sibling adapters so a
        // transient startup outage doesn't strand ctrader on paper.
        ("ctrader", _) => {
            let client: Box<dyn vike_exec::ExecutionClient + Send> =
                match vike_ctrader::mount::live_mount_for_account(
                    account,
                    vars,
                    symbol,
                    recon_enabled,
                    live_events,
                    halt_admit,
                ) {
                    Some(mount) => {
                        live = true;
                        recon = mount.recon;
                        // Live RiskGate from the handshake-resolved symbol grid (PR-2b): no extra
                        // network — the grid was resolved during the bridge's own exec handshake.
                        // An unknown symbol keeps the permissive default (byte-identical to
                        // pre-PR-2b behavior), same as the crypto arms' failed pre-fetch.
                        if let Some(f) = mount.symbols.risk_properties(symbol) {
                            limits = vike_exec::RiskLimits::from_properties(&f);
                        }
                        // …and the SAME table answers for every DECLARED LEG, at no network cost:
                        // `risk_properties` reads the handshake's own `SymbolsList` (its doc says
                        // "Needs NO network"), so a second FX pair costs one map lookup. This is an
                        // arm `declared_grid_source` classifies `InHand`; an unknown leg name simply
                        // gets no row and is reported by `warn_ungridded_legs` below.
                        symbol_grids =
                            symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
                                mount.symbols.risk_properties(leg)
                            });
                        mount.client
                    }
                    // Absent/half-written credentials, or a failed handshake, both resolve `None` —
                    // paper (byte-identical to the `_` fallback below; `recon` stays whatever it was
                    // before this arm ran, i.e. `None`). This is the INERT-DEFAULT path proven for
                    // every roster venue by `all_roster_venues_absent_creds_stay_paper_and_inert`.
                    None => Box::new(paper_client(venue, symbol, static_default)),
                };
            client
        }
        // Alpaca uses an OAuth2 client-credentials pair (ALPACA_SANDBOX_{CLIENT_ID,CLIENT_SECRET,
        // ACCOUNT_ID}), NOT the generic `{VENUE}_DEMO_API_KEY` shape, so `load_credentials_from`
        // above returns `None` for it — it self-gates on `load_alpaca_config_from(Demo, …)` (like the
        // aster/hyperliquid/ctrader arms match on `_`). This is the EXEC + inline-recon mount:
        // market data comes from the separate `AlpacaDataClient` feed the daemon wires elsewhere
        // (`wire_venue_feeds`, as for ctrader above).
        //
        // Unlike ctrader, `AlpacaExecutionClient::spawn` is INFALLIBLE at mount time — it spawns a
        // self-reconnecting `ExecActor` + SSE reader (no blocking startup handshake), so there is NO
        // connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.
        ("alpaca", _) => {
            match vike_alpaca::load_alpaca_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    live = true;
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
        // feed the daemon wires elsewhere (`wire_venue_feeds`' IG arm); a mount with no bar feed is
        // paper-INERT (the paper client never fills), which is what an absent-creds IG was in
        // `vike-app`, which never wired one. IG uses the `IG_DEMO_*`
        // config shape, NOT the generic `{VENUE}_DEMO_API_KEY` one, so `load_credentials_from` above
        // returns `None` for it — the arm self-gates on `load_ig_config_from(Demo, …)` (like the
        // aster/hyperliquid/ctrader/alpaca arms match on `_`). `IgExecutionClient::spawn` is INFALLIBLE
        // at mount time (an ExecActor + a background Lightstreamer stream that each self-gate on their
        // own login — no blocking startup handshake, so there is NO connect-failure paper-demotion
        // branch, unlike ctrader/ibkr): present creds ⇒ live, absent ⇒ paper.
        ("ig", _) => {
            match vike_ig::load_ig_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    live = true;
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
                    recon = vike_bridge_core::venue_mount::recon_if_enabled(recon_enabled, || {
                        vike_ig::recon_client(&cfg, symbol)
                    });
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
        // the go-live machinery the CEX venues and hyperliquid have (decision 0095: their network
        // IS the ceiling, via `ceiling_selects_mainnet`/`Env::for_ceiling`), the `⚠ REAL-MONEY`
        // line, and the budget refusal keyed to it — none of which oanda has grown on its own, and
        // growing it is a shared capability-table extension, by one coordinated PR informed by
        // every consumer, never edited in passing.
        //
        // `error!`, not `warn!`, and for the reason the passphrase report above states: this is the
        // class of an unreadable store — credentials that EXIST and cannot be used — and a
        // misconfiguration wearing the "not configured" answer looks exactly like a correct fresh
        // install. The NAMES are logged; the token never is (`UnreachableLiveTier` holds no value).
        ("oanda", _) => {
            match vike_oanda::mountable_tier_for_account(account, vars) {
                vike_oanda::MountableTier::Practice(cfg) => {
                    live = true;
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
                    recon = vike_bridge_core::venue_mount::recon_if_enabled(recon_enabled, || {
                        vike_oanda::recon_client(&cfg, symbol)
                    });
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
        // ── DUKASCOPY ────────────────────────────────────────────────────────────────────────────
        //
        // ⚠ THIS ARM DID NOT EXIST UNTIL 2026-09-09, and its absence was invisible: the bridge, the
        // Java sidecar, the exec client and the recon client were all written and tested, and
        // `crates/vike-mount/src/lib.rs`'s own `NO_LIVE_ARM` table recorded the venue as "ships an
        // exec factory, and `make_engine` has no arm that mounts it". So a box could hold working
        // Dukascopy credentials, a provisioned JRE and the bridge jar and still not trade the venue
        // by any configuration — because this crate did not even depend on the bridge.
        //
        // NOT feature-gated, unlike ibkr/fxcm/polymarket below: there is no native SDK and no
        // optional dependency, so the arm compiles everywhere and self-gates on credentials like
        // the REST venues above.
        //
        // ⚠ IT OWNS A CHILD PROCESS, which is what makes this arm's failure modes its own:
        //   * the JVM and the jar must BOTH resolve (`resolve_dukascopy_tools`). An absent jar is a
        //     warn-and-stay-paper inside `spawn`, never a hard failure — a box without the sidecar
        //     is an unconfigured box, not a broken one, exactly like absent credentials.
        //   * `spawn` is a BLOCKING, FALLIBLE start (fork + handshake), so like the ctrader and ibkr
        //     arms a failure DEMOTES the venue to paper for the whole session rather than retrying.
        //   * the tools directory is DERIVED from the boot's own state directory rather than
        //     re-walked, for the reason the ctrader arm gives: the `_from`-less resolvers are
        //     `$VIKE_SETTINGS_DIR`-blind, and every shipped unit sets that variable.
        //
        // ⚠ Its recon client is DERIVED FROM THE RUNNING EXEC CLIENT (`client.recon_client()`),
        // unlike every REST venue's `recon_client(&cfg, symbol)` factory: the sidecar owns the
        // connection, so there is no second socket to open. It covers POSITIONS only — see
        // `crates/bridges/dukascopy/src/recon_client.rs` for what it defers and why.
        ("dukascopy", _) => {
            // ⚠ THIS ARM ADDRESSES THE ACCOUNT IT IS MOUNTED FOR, and until 2026-09-15 it did not:
            // it hardcoded `DukascopyAccount::Demo1` and ignored its own `account` parameter, which
            // `crates/vike-mount/src/arming.rs`'s `arm_addresses_accounts` then covered for by
            // refusing the venue outright. Both halves are gone.
            //
            // WHICH account is decided by `vike_dukascopy::resolve_account` (reached through
            // `crate::dukascopy::resolve_in`, over this root's own store read), keyed on the
            // settings database's `account` row — its credential-key OWNER PREFIX picks the broker,
            // its `venue_account_id` is what an operator writes to address it, and its `label` is
            // used only if somebody wrote one (the owner refused the migration's provisional labels
            // twice). `vike_dukascopy::account`'s module doc carries the whole argument, including
            // why `AccountLabel::Default` is `Demo1` on every box whatever the database holds.
            //
            // ⚠ A REFUSAL IS A REFUSAL, never a fallback. DEMO1 is Dukascopy Bank SA and DEMO2 is
            // Dukascopy Europe IBS AS — two LEGAL ENTITIES — so an account the store cannot identify
            // stays PAPER and says so by name. Coercing it onto Demo1 would route an order to a
            // counterparty nobody chose.
            //
            // ⚠ AND WHICH ACCOUNT GETS THE PROCESS'S ONE SIDECAR is decided by the POLICY, above
            // the claim — `crate::exclusive::holds`, the same call
            // `venue_account_arming`'s dukascopy row makes, so this arm cannot mount an account the
            // projection said would stay paper (nor refuse one it said would arm). Until 2026-09-15
            // there was no such decision: the claim below was taken first-come, the fan-out always
            // reaches the DEFAULT account first, and no policy could give the sidecar to a labelled
            // account at all.
            match dukascopy::resolve_in(account, arming::directory_of(policy)) {
                Err(refusal) => {
                    // `error!`, the same class as a half-written credential set: the operator wrote
                    // a configuration that describes a real account and it is not being traded.
                    tracing::error!(venue, "{refusal}");
                    Box::new(paper_client(venue, symbol, static_default))
                }
                Ok(mount) => {
                    match vike_dukascopy::load_dukascopy_config_from(mount.account, vars) {
                        // ⚠ THE HOLDER CHECK SITS HERE, below the credential read and above the
                        // claim, so the line an operator gets is exact: an account with no
                        // credentials was never going to arm and is told nothing (the `None` arm
                        // below, unchanged), while an account that WOULD have armed is told what it
                        // lost and to whom.
                        Some(_)
                            if !crate::exclusive::holds(registry, venue, account, vars, policy) =>
                        {
                            let refusal =
                                dukascopy::DukascopySidecarRefusal::SidecarHeldByAnother {
                                    label: account.to_string(),
                                    holder: crate::exclusive::holder(registry, venue, vars, policy)
                                        .to_string(),
                                };
                            tracing::error!(venue, "{refusal}");
                            Box::new(paper_client(venue, symbol, static_default))
                        }
                        Some(cfg) => {
                            // ⚠ ONE SIDECAR PER PROCESS, claimed HERE — after the credentials, the
                            // account AND the holder decision, immediately before the spawn.
                            // Claiming it any earlier would let a mount that fails for an unrelated
                            // reason (absent credentials, an account this process does not hold the
                            // sidecar for) burn the claim and leave the venue unmountable for the
                            // rest of the process. `crate::dukascopy`'s module doc carries the
                            // evidence — a shared JForex platform cache whose corruption takes down
                            // EVERY login, and a 300s `READY_TIMEOUT` paid serially per sidecar —
                            // and the measurement that retires the refusal.
                            //
                            // ⚠ The claim is a GUARD that releases on drop unless `keep()` commits
                            // it, and that is a fix rather than a flourish: `spawn` fails on four
                            // ordinary paths (no jar, `Command::spawn`, a `Fatal` envelope from a
                            // bad login, the ready timeout), and a burned claim then refused every
                            // later account in the process with a reason that was not true —
                            // `crates/bridges/dukascopy/CLAUDE.md` records the JNLP 404 that makes
                            // that the common path, not the rare one.
                            match crate::exclusive::claim(venue) {
                                None => {
                                    let refusal =
                                        dukascopy::DukascopySidecarRefusal::SidecarAlreadyClaimed {
                                            label: account.to_string(),
                                        };
                                    tracing::error!(venue, "{refusal}");
                                    Box::new(paper_client(venue, symbol, static_default))
                                }
                                Some(claim) => {
                                    // `<project>/settings/state`, the ONE walk the boot already
                                    // performed and handed over — i.e.
                                    // `vike_model::state_path::project_state_dir_from`'s answer,
                                    // `$VIKE_SETTINGS_DIR` included. Re-walking here would be the
                                    // `_from`-less blindness this arm's header warns about.
                                    let project_state =
                                        vike_bridge_core::halt::declared_project_state_dir();
                                    // `<project>/bin`, derived from that same state directory.
                                    let project_bin = project_state.as_deref().and_then(|state| {
                                        // `<project>/settings/state` -> `<project>`
                                        state
                                            .parent()
                                            .and_then(std::path::Path::parent)
                                            .map(|p| p.join("bin"))
                                    });
                                    // ⚠ THE STATE DIRECTORY IS PASSED IN ITS OWN RIGHT, never
                                    // re-derived from `project_bin` above: `$VIKE_SETTINGS_DIR`
                                    // moves `settings/` without moving `bin/`, so the two are a
                                    // pair of independent facts on exactly the boxes that set it.
                                    // It resolves the JVM's `user.home` — see
                                    // `crates/bridges/dukascopy/src/exec.rs`'s module doc for the
                                    // `ProtectHome=yes` defect that makes it load-bearing here.
                                    let tools = vike_dukascopy::resolve_dukascopy_tools(
                                        vars,
                                        project_bin.as_deref(),
                                        project_state.as_deref(),
                                    );
                                    match vike_dukascopy::DukascopyExecutionClient::spawn(
                                        cfg,
                                        &tools,
                                        live_events.clone(),
                                    ) {
                                        Ok(client) => {
                                            // THE SIDECAR IS RUNNING — commit the claim for the life of
                                            // the process. Every other path out of this block drops the
                                            // guard, which RELEASES it: a failed spawn must not refuse
                                            // the next account with "this process already runs a
                                            // sidecar" when it runs none.
                                            claim.keep();
                                            live = true;
                                            // ⚠ The line NAMES THE BROKER, the row and the book. That is the
                                            // only moment an operator can catch a wrong mapping before an
                                            // order does, and none of the three is a secret: a
                                            // `venue_account_id` is the number the venue prints on its own
                                            // page and a row id is an opaque integer. The login and the
                                            // password appear nowhere — `DukascopyConfig`'s `Debug` redacts
                                            // the password and this does not print the config at all.
                                            tracing::warn!(
                                                venue,
                                                account = %account,
                                                broker = mount.account.broker(),
                                                keys = mount.account.key_prefix(),
                                                row = ?mount.row,
                                                book = ?mount.book,
                                                "dukascopy: DEMO credentials present and the JForex sidecar \
                                                 started → LIVE exec client (real demo-account orders)"
                                            );
                                            // ⚠ THE HANDSHAKE HALF, and the one moment the VENUE's
                                            // own answer about which account these credentials are
                                            // is knowable. The sidecar's ready envelope carries it
                                            // and this arm used to throw it away; the
                                            // credential-schema spec's §4.5 records
                                            // `venue_account_id` and `last_verified_at` as columns
                                            // nothing in this tree writes, and this is the input
                                            // that closes both.
                                            //
                                            // It PARKS rather than writes: the shipped unit's
                                            // ProtectSystem=strict grants only
                                            // <project>/settings/state and the settings database is
                                            // outside it, so a mount-time UPDATE is EROFS on every
                                            // deployment. `vike-cli secrets confirm` folds it. A
                                            // DISAGREEMENT is reported at error! and writes nothing
                                            // — it never fails the mount, because the session
                                            // authenticated. `crate::dukascopy::record_confirmation`
                                            // carries the whole argument.
                                            //
                                            // ⚠ It sits AFTER `claim.keep()` deliberately: this
                                            // runs only on the path where a sidecar really
                                            // authenticated, so a parked record can never describe
                                            // a login that did not happen.
                                            dukascopy::record_confirmation(
                                                &mount,
                                                client.handshake_account(),
                                            );
                                            // Derived from the client, so it must be built BEFORE the client
                                            // moves into the box — the sidecar owns the one connection both
                                            // share.
                                            let rc = client.recon_client();
                                            recon = vike_bridge_core::venue_mount::recon_if_enabled(
                                                recon_enabled,
                                                || Some(Box::new(rc)),
                                            );
                                            Box::new(client)
                                        }
                                        Err(e) => {
                                            // Includes the absent-jar case, which `spawn` has already logged
                                            // with the path it looked for. Staying paper is the live gate
                                            // doing its job.
                                            tracing::error!(
                                                venue,
                                                account = %account,
                                                error = ?e,
                                                "dukascopy: the JForex sidecar would not start — staying \
                                                 PAPER"
                                            );
                                            Box::new(paper_client(venue, symbol, static_default))
                                        }
                                    }
                                }
                            }
                        }
                        None => Box::new(paper_client(venue, symbol, static_default)),
                    }
                }
            }
        }

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
                    // is intentionally not read. Recon breadth: ibkr joins the reconciled set behind
                    // this crate's `ibkr` feature, driven by the live root's reconcile driver
                    // (`vike-tradehub`; this said "venue #9 once the vike-app driver wiring lands"
                    // until 2026-09-28).
                    //
                    // LAZY (the `recon_enabled` gate): with reconciliation off the cpapi
                    // `tickle`/`secdef_search` handshake is never performed — the factory is not
                    // called. Enabled ⇒ the same call, in the same position (still BEFORE the exec
                    // connect below), so the connect-failure arm's `recon = None` demotion still
                    // covers it.
                    recon = vike_bridge_core::venue_mount::recon_if_enabled(recon_enabled, || {
                        vike_ibkr::recon_client::recon_client(&cfg, symbol)
                    });
                    match vike_ibkr::IbkrExecutionClient::connect(&cfg, live_events.clone()) {
                        Ok(client) => {
                            live = true;
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
        // `vike_fxcm::sdk_available()` opens the ForexConnect shim on THIS box and reports what
        // happened. Without it every `FxcmSession` call returns `Unavailable`, so
        // `FxcmExecutionClient::spawn` still hands back a perfectly ordinary exec client whose
        // session thread RETURNS AT LOGIN — after which every submit is accepted by the actor,
        // forwarded to a thread that is not there, and discarded in silence. That is the
        // no-silent-vanish half of the venue-adapter contract failing at the mount, so this arm
        // refuses it OUT LOUD and lands on paper, the same shape as the oanda live-tier refusal.
        //
        // ⚠ **THE GATE BECAME A RUNTIME ONE ON 2026-09-09, and this block used to say the
        // opposite** — that it reported "whether `build.rs` found the SDK and emitted its `fcsdk`
        // cfg". It did, and that made the answer a property of the BUILD BOX: fine while the only
        // fxcm binary was built and run on the same machine, and wrong the moment one binary is
        // built where the SDK is and shipped where it is not. `crates/bridges/fxcm/src/loader.rs`
        // opens `libfcshim.so` instead, so the same `vike-backend` that runs everywhere arms FXCM
        // exactly on the boxes that have the shim installed. There is no "stub build" any more.
        //
        // ⚠ The CI configuration of this arm is still the REFUSING one, on every runner and in
        // every feature lane: no CI machine stages the SDK, so none builds a shim for the loader to
        // find. What CI proves here is that the arm compiles and refuses — and it now proves rather
        // more than it did, because the FFI it guards is COMPILED on every runner instead of being
        // `#[cfg]`-ed away. That it TRADES is still proven only on a box with the SDK staged, by
        // hand, through `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`.
        //
        // ⚠ WEAKER ROBUSTNESS CONTRACT, and weaker than ctrader/ibkr's: `spawn` is INFALLIBLE, so
        // unlike those two there is no connect result to demote on. A bad password, an unreachable
        // gateway and a box with no shim are indistinguishable from outside (`crates/bridges/fxcm/
        // CLAUDE.md` records this as by-design), so the no-shim case is the only one this arm can
        // catch — a live-but-failing login mounts "live" and trades nothing. Reconcile is what
        // notices; see the rollout note in the root CLAUDE.md.
        #[cfg(feature = "fxcm")]
        ("fxcm", _) => {
            match vike_fxcm::load_fxcm_config_for_account(Environment::Demo, account, vars) {
                Some(cfg) => {
                    if vike_fxcm::sdk_available() {
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
                        recon =
                            vike_bridge_core::venue_mount::recon_if_enabled(recon_enabled, || {
                                vike_fxcm::recon_client(&cfg, symbol)
                            });
                        live = true;
                        tracing::warn!(
                            connection = %cfg.connection,
                            // WHICH RUNG answered, not merely that one did — and rung is the right
                            // word. ⚠ The loader's ladder ENDS at the dynamic loader's own search,
                            // whose rung is the bare file name, so on a box that took that rung
                            // this field reads `libfcshim.so` and names no file: `ldd` on the
                            // running binary is what resolves it. On a box with an installed
                            // `<root>/lib/libfcshim.so` — rungs 1 and 2 — it is a real path and it
                            // is the only record of which file the session was made through. The
                            // raw value is logged rather than
                            // `vike_fxcm`'s operator-facing rendering of it because a structured
                            // field should carry the value, not a sentence about it.
                            shim = vike_fxcm::sdk_shim_path().unwrap_or("<unknown>"),
                            // WHAT THAT SHIM CAN DO, asked at the mount rather than at the first
                            // failure. `0` means it predates `fc_login_ex`, so on this box a login
                            // failure can report THAT it failed and never why — and the remedy is
                            // an operator action (`just fxcm-package <root> <shim>`), which
                            // is the worst thing to learn mid-incident. It is NOT a reason to
                            // refuse the mount: an old shim executes orders exactly as it always
                            // did, and the whole point of binding the new symbols optionally is
                            // that a diagnostic can never cost a venue.
                            shim_abi = vike_fxcm::sdk_abi_version().unwrap_or(0),
                            "fxcm: config present and the ForexConnect shim loaded → LIVE exec \
                             client (real orders on the resolved account)"
                        );
                        // No live RiskGate pre-fetch: FXCM exposes no symbol-properties endpoint short
                        // of a session-bound SDK call, so `limits` keeps the permissive default —
                        // byte-identical to a failed pre-fetch on the other arms.
                        Box::new(vike_fxcm::FxcmExecutionClient::spawn(cfg, live_events.clone()))
                    } else {
                        // THE REFUSAL. Paper, `recon` stays `None` — the same inert outcome an
                        // unconfigured venue reaches, arrived at LOUDLY instead of silently.
                        //
                        // ⚠ It carries the loader's own diagnostic since 2026-09-09, and that is
                        // the whole reason `sdk_unavailable_reason` exists. The remedy used to be
                        // "rebuild on a box with the SDK", which a message could simply state; it
                        // is now "install the shim where this process looks", and the two states an
                        // operator confuses — FXCM is not set up here, versus FXCM is set up and
                        // the file is in the wrong place — are told apart only by the list of paths
                        // that were tried. A bare refusal reads as the first whichever it is.
                        tracing::error!(
                            venue,
                            reason = vike_fxcm::sdk_unavailable_reason().unwrap_or("<none>"),
                            "fxcm: credentials are present but the ForexConnect shim did not load \
                             on this box, so a live mount would accept every order and discard it \
                             in silence. REFUSING the live mount and staying paper. `reason` lists \
                             every path that was tried (see \
                             crates/bridges/fxcm/scripts/provision-fcsdk.sh and \
                             docs/ops/fxcm-forexconnect.md)"
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
        // `recon` stays `None` (paper venues never get a reconcile handle).
        _ => Box::new(paper_client(venue, symbol, static_default)),
    };
    Ok(MountParts {
        client,
        recon,
        limits,
        contract_size,
        default_margin_mode,
        symbol_grids,
        grid_source: symbol_grid::legacy_grid_source(venue),
        record_tier: Some(if mainnet {
            vike_config::VenueMode::Live
        } else {
            vike_config::VenueMode::Demo
        }),
        identity: hyperliquid_master.filter(|(_, master)| master.confirmed).map(
            |(is_live, master)| {
                (
                    master.address,
                    "`userRole`",
                    if is_live {
                        vike_config::VenueMode::Live
                    } else {
                        vike_config::VenueMode::Demo
                    },
                )
            },
        ),
        live,
        static_default,
    })
}

/// **THE SHARED TAIL** — what every venue's mount parts become an engine through: the declared-leg
/// grid, the operator budget, the universal defaults, the exposure and equity ceilings, the
/// live-budget backstop, the fee schedule, the identity record and the engine itself. Moved
/// unchanged out of [`make_engine_for_account`], where it followed the legacy match, so that the
/// legacy arms and the contract path fold through ONE tail (docs/decisions/0096).
///
/// It opens with the two facts each arm used to state for itself: the route key enters
/// `live_venues` when the mount went LIVE, and the halt-admit ARMED report is said once the
/// outcome is known.
#[allow(clippy::too_many_arguments)]
fn assemble_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    parts: MountParts,
    live_venues: &mut HashSet<String>,
    route_key: String,
    halt_admit: vike_model::HaltAdmit,
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&MountPolicy>,
) -> Result<EngineAndRecon, MountError> {
    let MountParts {
        client,
        recon,
        mut limits,
        contract_size,
        default_margin_mode,
        symbol_grids,
        grid_source,
        record_tier,
        identity,
        live,
        static_default,
    } = parts;
    if live {
        live_venues.insert(route_key.clone());
    }
    // The ARMED/NOT-ARMED half of the halt-admit report, said HERE because here is the first point
    // where the answer is known: `report_halt_admit` runs before credentials and before any
    // blocking handshake, both of which can land a venue on PAPER. See that function for why
    // announcing `verify` from up there was a claim the mount could not keep. Silent at every venue
    // but cTrader: `vike_model::effective_halt_admit` degrades `verify` to `admit` everywhere else,
    // and `admit` logs nothing.
    report_halt_admit_armed(venue, halt_admit, live);
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
    symbol_grid::warn_ungridded_legs(
        venue,
        grid_source,
        symbol,
        declared_legs,
        &limits.grid_by_symbol,
        &limits,
    );
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
    // ─── …AND THE SAME CEILING, STATED FOR THIS ONE ACCOUNT ───────────────────────────────────
    //
    // `policy.account_exposure.<venue>.<LABEL>`, folded through the SAME `narrow_account_exposure`
    // immediately after the box-wide figure. Two folds rather than a choice between them, and that
    // is the whole design: `narrow_account_exposure` is a `min`, so the pair composes to
    // `min(box, account)` with either side absent falling through — which makes "an account line
    // can only ever TIGHTEN" a property of the OPERATION rather than of this call site getting the
    // precedence right.
    //
    // ⚠ WHY IT EXISTS. The box-wide figure is ONE number applied to every engine, and this function
    // runs once per `(venue, AccountLabel)` — so ten accounts meant one number applied ten times,
    // with no way to say "this one gets less". That is not the shared-BOOK hazard the warning above
    // describes (two engines over one ledger); it is the ordinary case, where ten accounts really
    // are ten books and the operator still wants ten different limits.
    //
    // ⚠ The DEFAULT account can be named here, which is the difference from the `[accounts]` mode
    // table — `vike_config::VenuePolicy::account_exposure`'s doc carries the argument. So on a
    // single-account box this is reachable too, and is not a multi-account-only feature.
    limits.narrow_account_exposure(policy.and_then(|p| p.venues.account_exposure(venue, account)));
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
    // ─── WHICH ACCOUNT DID THIS MOUNT JUST AUTHENTICATE AS? ───────────────────────────────────
    //
    // ONE site for every venue, and it can be one site because the seam it uses defaults to
    // *nothing*: a venue whose `ReconClient` has not implemented `fetch_account_identity` issues no
    // request and says nothing, and a PAPER mount built no `ReconClient` for it to ask.
    // `book_identity::record_authenticated_account` carries the per-venue cost, why this is not gated on
    // `recon_enabled`, and why a failure is a warning rather than a refused mount.
    //
    // ⚠ **LOAD-BEARING ORDERING: AFTER `resolve_fee_schedule`, NEVER BEFORE IT.** On binance SPOT
    // the account id and the commission rates ride ONE body — `/api/v3/account` — and
    // `vike_binance::family::FamilyReconClient` remembers the id from whichever call pulls it
    // first. Asking here FIRST would make this rung fetch that body itself and leave the fee read
    // to fetch it AGAIN: two signed reads where the venue's own answer already carried both. Placed
    // after, the fee resolution has filled the cell and this rung costs binance nothing — which is
    // what `book_identity`'s own row claims, and the claim was wrong for one commit because this
    // block sat directly under the venue match instead.
    //
    // okx and deribit are unaffected either way: nothing else on those clients reads their account
    // endpoint, so their read is genuinely new wherever this sits.
    //
    // ⚠ This is the CEX half of what the block just below (hyperliquid's own identity confirmation)
    // does. The `("hyperliquid", _)` arm above asks `userRole` itself and folds the answer through
    // the same recorder there; the difference is that HL can compute its book offline from the key
    // and the CEX venues cannot — `book_identity`'s bybit/okx rows say the store names no account
    // and only an authenticated call can. This is that call, for the venues where it is the only
    // one there is.
    if let Some(tier) = record_tier {
        book_identity::record_authenticated_account(
            venue,
            account,
            tier,
            recon.as_deref(),
            arming::directory_of(policy),
        );
    }
    // Hyperliquid's own identity confirmation (decision 0088's B3): the bridge's `userRole` probe
    // already ran, inside its arm above, and decided whether the answer is worth recording — see
    // `vike_hyperliquid::mount::MasterOutcome`'s doc. Only a CONFIRMED answer is parked here; an
    // unanswered or contradicted probe records nothing, mirroring `record_confirmation`'s own
    // `Disagrees` arm, which writes nothing either.
    if let Some((book, evidence, tier)) = identity {
        book_identity::record_confirmation(
            venue,
            evidence,
            account,
            tier,
            &book,
            arming::directory_of(policy),
        );
    }
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
mod fee_schedule_tests;

/// Polymarket's own reconcile composition — `poly_recon_wanted`'s four rows, the venue gated TWICE.
///
/// ⚠ This module also pinned the GLOBAL reconcile gate's laziness (`recon_if_enabled`) until the
/// venue mount contract moved that helper into `vike_bridge_core::venue_mount`, where its tests
/// went with it (`crates/bridges/ctrader/src/mount.rs` still re-states the same laziness, covered by
/// its own offline tests). What is left is feature-gated, so the module is too: a default build
/// has nothing here to compile.
#[cfg(all(test, feature = "polymarket"))]
mod recon_gate_tests;

/// PRE-CONNECT live-intent probe + refusal (the #817 "refusal happens POST-connect" residual):
/// `would_mount_live` must recognize exactly the live arms' own credential shapes — via the SAME
/// loaders the arms call — and the budget refusal must fire on it BEFORE any venue session.
#[cfg(test)]
mod preconnect_tests;

#[cfg(test)]
mod account_event_lane_tests;

/// The generic fold over a `VenueRow::Mount` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no bridge implementing the contract yet (docs/decisions/0096).
#[cfg(test)]
mod contract_tests;
