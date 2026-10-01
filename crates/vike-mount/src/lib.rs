//! vike-mount — the GENERIC venue mount: the fold every venue's `ExecutionEngine` is built
//! through, with no venue named in it
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! [`make_engine`] is the one entry point. It is handed the REGISTRY —
//! `vike_tradehub::registry::REGISTRY`, one [`VenueRow`] per `vike_model::VENUES` id, which
//! [`build_node`] receives as [`NodeConfig::registry`] — and for the venue asked for it
//! runs, in order:
//!
//! 1. **The arming ceiling** (`policy.venues.<venue>`, per account), consulted FIRST: a `paper`
//!    venue returns the paper engine having loaded no credential and opened no socket. The
//!    ceiling can only ever REFUSE (`vike_config::VenueMode::cap` is `min`) — except that for the
//!    four venues in `vike_secrets::live_means_mainnet::SWITCHED_VENUES` it also CHOOSES the
//!    network (decision 0095) — and it reaches a bridge only as `live_permitted: bool`.
//! 2. **The arming probe** — the bridge's `VenueMount::resolve`, PURE, behind two generic
//!    preconditions (a labelled account on a venue whose declaration addresses no named account;
//!    a `paper` ceiling). The arming screen reads the same function, so projection and mount
//!    agree.
//! 3. **The pre-connect budget refusal** for a venue the probe calls armed.
//! 4. **The process-exclusive claim**, for a venue whose declaration names one
//!    (`crates/vike-mount/src/exclusive.rs`'s `holder` and `claim`; dukascopy's JForex sidecar is
//!    the one today), kept only when the bridge's mount comes back `Live`.
//! 5. **The bridge's `VenueMount::mount`, and the fold of its outcome**: the grid and the
//!    declared legs' grids, the contract size and margin mode, the operator budget and universal
//!    defaults, the exposure and equity narrowing, the fee schedule, the identity record at the
//!    tier the credentials bound — and a PAPER engine whenever the bridge declines, because only
//!    this crate builds one.
//!
//! A venue this build does not compile (`VenueRow::FeatureAbsent`) mounts paper and answers the
//! generic facts of `crates/vike-mount/src/registry.rs`'s `ABSENT_CLOCK`; an id the registry does
//! not carry mounts paper too.
//!
//! ⚠ **This crate names no venue, and two gates hold it.** At layer 35 any edge to a bridge
//! (40/41) points up and `crates/vike-ops/tests/layer_gate.rs` refuses it; and
//! `crates/vike-ops/tests/mount_names_no_venue_gate.rs` refuses a `vike_model::VENUES` id as a
//! string literal anywhere in this crate's non-test source — the node assembly and strategy mounts
//! included. A venue's facts live in its bridge's `VenueDeclaration`; its roster tests run in
//! `vike-tradehub`, which holds the registry and the wired markets.
//!
//! Settings: [`make_engine`] takes a [`MountPolicy`] — the projection of `vike_config::Policy`
//! this mount APPLIES, plus the settings database's account table and venue settings the
//! composition root read. This crate never reads `std::env` for it, and `None` reads PAPER.
//!
//! Startup safety: [`preflight`] is the pure go/no-go gate and [`startup`] its real-probe wiring,
//! run once by [`build_node`] — the clock leg over each registry row's declared clock, the
//! credential leg over each row's `credential_probe`.
//!
//! # The node assembly and the strategy mounts
//!
//! The run/composition layer stacked on the fold — the `vike-run` crate until it merged into this
//! one (docs/decisions/0098-vike-run-merges-into-vike-mount.md), which moved the crate from layer
//! 30 to 35 (its highest normal edge is now `vike-core`):
//!
//! - [`build_node`] (`src/node.rs`): one engine per row of the composition root's wired markets
//!   ([`NodeConfig::markets`], [`WiredMarket`] — `vike-tradehub`'s table) through [`make_engine`],
//!   mounted in table order and held in engine-rank order, the reconcile legs, the single-writer
//!   core and the live-event forwarder; plus the pre-mount armed set ([`armed_live_venues`]) the
//!   live-account locks are claimed from, and the `venue_mounted` journal records.
//! - The strategy mounts (`src/run.rs`): the A-S maker and any strategy on the live core, over the
//!   paper exchange ([`build_paper_maker_core`], [`build_paper_strategy_core_with`], …) or over
//!   [`build_node`] ([`build_live_maker_core`], [`build_live_strategy_core`], …).
//! - The cross-exchange maker mount (`src/xemm.rs`, [`build_paper_xemm_core`],
//!   [`build_live_xemm_core`]).
//! - The [`incident`] evidence collector, which the `incident` bin drives.
//!
//! None of it names a venue: the wired markets are rows the daemon hands in, the way the registry is.

use std::collections::{HashMap, HashSet};

use vike_model::account_keys::AccountLabel;

use arming::{
    account_ceiling, account_event_sender, account_route_key, arm_universal_defaults,
    ceiling_permits_live, multiplier_grid, report_unaddressable_accounts, venue_ceiling,
};
use paper_fallback::{
    paper_engine, report_capped_to_paper, report_halt_admit, report_halt_admit_armed,
    venue_arming_migration,
};

mod arming;
pub mod book_identity;
mod contract;
mod error;
mod exclusive;
mod paper_fallback;
pub mod policy;
pub mod preflight;
mod registry;
pub mod server_time;
pub mod startup;
pub mod symbol_grid;

pub use arming::{
    known_accounts, margin_mode_grid, require_live_risk_budget, resolve_fee_schedule,
    shared_book_ceiling_note, shared_books_for, symbol_for_account, unaddressable_accounts_message,
    unaddressable_accounts_text, venue_account_arming, venue_arming, venue_arming_under,
    would_mount_live, would_mount_live_under, would_mount_live_under_policy,
};
// `paper_fallback` is private, so this is the only name for the migration warning's text — the
// roster tests in `crates/vike-tradehub/tests/mount_roster.rs` read it (docs/decisions/0096).
pub use paper_fallback::venue_arming_migration_message;
pub use policy::MountPolicy;
// Crate-root vocabulary: `registry` is private, so this is the only name for the row type and its
// lookup (docs/decisions/0096).
pub use registry::{VenueRow, row_of};
pub use symbol_grid::declared_grid_source;
/// The arming VOCABULARY, re-exported because it appears in THIS crate's public signatures.
///
/// [`venue_arming`] returns `Vec<vike_config::VenueArming>`, so a consumer can already hold these
/// values — it just could not NAME them without taking a `vike-config` dependency of its own.
/// `vike-run` needed exactly that to write the `venue_mounted` journal records (before it merged into
/// this crate, docs/decisions/0098), and a manifest edge
/// added for a type name is worse than the re-export: it widens the dependency graph to say
/// something the signature already says.
///
/// ⚠ Not a `pub use` SHIM in the sense `CLAUDE.md` forbids — nothing MOVED here and no old spelling
/// is being kept alive. This is the module-vocabulary exception that rule names, the same shape as
/// `vike_exec::ExecutionClient`.
pub use vike_config::{ArmingBlock, VenueArming, VenueMode, VenuePolicy};

// ⚠ `vike_mount::halt` (a re-export of `vike_bridge_core::halt`) is GONE. It existed so `vike-run`,
// which built its own paper mount outside `make_engine`, could arm the SAME operator HALT sentinel
// without a `vike-bridge-core` edge of its own; vike-run merged into this crate
// (docs/decisions/0098), whose normal `vike-bridge-core` edge already names it, so every caller
// spells `vike_bridge_core::halt` and the second name is not kept.

// ── The node assembly and the strategy mounts (vike-run's, merged in by docs/decisions/0098) ──
// The modules are PRIVATE and their public items are re-exported here, so each has exactly one
// public name (`vike_mount::X`); `incident` stays a public module because its bin addresses it by
// module path.
pub mod incident;
mod node;
mod run;
mod xemm;

pub use node::{
    MOUNT_FAILED, MountAccount, Node, NodeConfig, NodeError, WiredMarket, account_symbols_for,
    armed_live_venues, build_node, build_node_with_preflight, journal_venue_mounts, mount_accounts,
    refuse_unarmed_live_venues,
};
pub use run::{
    BookFills, LiveMakerMount, MakerBreaker, MakerMount, MakerMountConfig, MakerSink, MakerSkew,
    MountSpec, MultiStrategyMount, PaperHalt, PaperMountOpts, StrategyMountSpec,
    TickBarSynthesizer, build_live_maker_core, build_live_multi_strategy_core,
    build_live_strategy_core, build_live_strategy_core_with_preflight, build_maker,
    build_paper_maker_core, build_paper_maker_core_with, build_paper_multi_strategy_core_with,
    build_paper_strategy_core_with,
};
/// The concrete maker [`build_maker`] returns. Re-exported because a caller that builds the A-S
/// maker through this crate must be able to NAME what came back — `vike-tradehub` holds it in an
/// `-> Option<SpreadMaker>` so its default mount and its `[strategy] name = "spread_maker"` mount
/// are provably ONE construction — without taking a direct `vike-mm` edge to do it. Costs nothing:
/// vike-mm is already in every consumer's tree through this crate. Vocabulary carried over from
/// vike-run's root, unchanged in meaning.
pub use vike_mm::SpreadMaker;
/// The paper exchange's fill record, which [`MakerMount::fills`] and the xEMM mounts expose.
/// Vocabulary carried over from vike-run's root, unchanged in meaning.
pub use vike_paper::PaperFill;
pub use xemm::{
    LiveXemmMount, PaperXemmMount, XemmConfigError, XemmMountConfig, XemmMountError,
    build_live_xemm_core, build_paper_xemm_core, build_paper_xemm_core_with, build_xemm_maker,
};

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
/// `crates/vike-mount/tests/risk_budget_diagnostic.rs` reads this path back OUT of the rendered
/// message and parses the real file as a `vike_core::RunProfile` (this crate, which absorbed vike-run
/// by docs/decisions/0098, is the lowest
/// crate that depends on BOTH halves), so this reference cannot rot into a dangling one.
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
/// [`build_node`]'s), and it must be BYTE-IDENTICAL — so it keeps its signature and reaches
/// this function with an EMPTY slice, which provably touches nothing (`declared_symbol_grids`
/// returns an empty map without calling the venue at all, and an empty `grid_by_symbol` carries
/// `skip_serializing_if`, so even `vike_exec::engine_snapshot::state_hash` is unchanged). Same
/// shape, and the same reason, as `crates/bridges/bybit/src/exec.rs`'s
/// `fetch_bybit_properties_with_cap` beside its plainer twin: widen the caller that needs the extra
/// fact, leave the ~19 that do not untouched.
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
/// follow in label order. [`build_node`] binds `[0]` to the engine it has always bound and
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
/// ([`account_symbols_for`] is the derivation). It replaced a single `symbol: &str`
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
    // for, carries no `vike_tradehub::wired_markets::WIRED_MARKETS` row, so a line emitted from its own mount would
    // be a line nobody ever reads. The message names every refused account at once and is
    // `Once`-latched, exactly like `venue_arming_migration` below.
    //
    // ⚠ Since 2026-09-15 it names NOTHING on any box: dukascopy was the last venue whose arm could
    // not address a second account, and every roster venue now addresses its accounts through its
    // bridge's declaration (`addresses_accounts`; a legacy row did through the hand-written
    // `arm_addresses_accounts` until the dukascopy port deleted both). The call stays because a
    // venue scaffolded by `just new-venue` declares `addresses_accounts: false` in its bridge's
    // `mount.rs` until its `resolve` and `mount` read the NAMED account's keys, so the next venue
    // to need this message is the next venue to exist.
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
    // test drives the resolution and `crates/vike-tradehub/tests/shared_book_report.rs` drives
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
/// was measured GREEN across this crate and `vike-run` (a separate crate then), because reaching
/// that loop needs two ACTIVE accounts and therefore a real socket.
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
    // production mount does pass `Some` (`crate::build_node` from `NodeConfig::policy`, and
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
        // assertion in `crates/vike-tradehub/tests/account_fanout.rs` actually exercise
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
    // THE VENUE'S OWN HALF: a contract row mounts through its bridge's `VenueMount`, and every
    // other row is the paper client. Both produce ONE shape, `MountParts`, so the shared tail below
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
        live_venues,
        route_key,
        halt_admit,
        risk_profile,
        policy,
    )
}

/// What a venue's mount produced, before the shared tail folds it — the one shape the contract path
/// and the paper client both produce (the legacy arms produced it too, until the last of them moved
/// into its bridge), so the tail cannot drift between them.
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

/// **THE SHARED TAIL** — what every venue's mount parts become an engine through: the declared-leg
/// grid, the operator budget, the universal defaults, the exposure and equity ceilings, the
/// live-budget backstop, the fee schedule, the identity record and the engine itself. Moved
/// unchanged out of [`make_engine_for_account`], where it followed the legacy match, so that the
/// legacy arms and the contract path folded through ONE tail while both existed
/// (docs/decisions/0096).
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
    // `crates/bridges/binance/src/family/recon.rs`'s `FamilyReconClient` remembers the id from
    // whichever call pulls it first. Asking here FIRST would make this rung fetch that body itself
    // and leave the fee read to fetch it AGAIN: two signed reads where the venue's own answer
    // already carried both. Placed after, the fee resolution has filled the cell and this rung
    // costs binance nothing — which is what `book_identity::record_authenticated_account`'s
    // per-venue cost list claims, and the claim was wrong for one commit because this block sat
    // directly under the venue match instead.
    //
    // okx and deribit are unaffected either way: nothing else on those clients reads their account
    // endpoint, so their read is genuinely new wherever this sits.
    //
    // ⚠ This is the CEX half of what the block just below (hyperliquid's own identity confirmation)
    // does. Hyperliquid's bridge asks `userRole` itself, inside its own mount since
    // docs/decisions/0096, and its answer reaches the same recorder there as the contract's
    // `IdentityReport`; the difference is that HL can compute its book offline from the key
    // and the CEX venues cannot — each CEX bridge's declaration (docs/decisions/0096) says the
    // store names no account and only an authenticated call can.
    // This is that call, for the venues where it is the only one there is.
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
    // already ran, inside `crates/bridges/hyperliquid/src/mount.rs`'s `HyperliquidVenueMount`
    // (docs/decisions/0096), and decided whether the answer is worth recording — see that file's
    // `MasterOutcome`; it arrives here as the contract's
    // `IdentityReport`, through `contract`'s `parts_from_outcome`. Only a CONFIRMED answer is
    // parked here; an unanswered or contradicted probe records nothing, mirroring
    // `record_confirmation`'s own `Disagrees` arm, which writes nothing either.
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
/// `crates/bridges/deribit/src/ratelimit.rs`); every other venue's own gate sits looser still (OKX
/// 50/2s ≈ 25/s, Bybit 18/s, Binance spot 90/10s ≈ 9/s and perp 270/10s = 27/s, Aster spot 90/min =
/// 1.5/s and perp 1080/min = 18/s — every one of them a row in that same table).
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

// Only reached (via `super::`) from `paper_mount_halt_tests`, a `#[cfg(test)]` module — an
// unconditional import here would go unused, and therefore warn, in a non-test build
// (`paper_client`'s last non-test caller in this file was the legacy arms). Kept HERE, below every
// documented constant in this file (`ARMED_MAX_ORDERS_PER_WINDOW` above), rather than at the top:
// `crates/vike-ops/tests/docs_constants_gate.rs`'s `code_only` stops reading a source file at its
// first INLINE `#[cfg(test)]` item (a `#[cfg(test)] use ...;` counts), so an import like this one
// placed near the top of the file hid `ARMED_MAX_ORDERS_PER_WINDOW` from that gate — measured red
// on the CI box (`every_claimed_default_still_has_that_value_in_the_code`) once two such imports
// existed above it.
#[cfg(test)]
use paper_fallback::paper_client;

#[cfg(test)]
mod paper_mount_halt_tests {
    /// Every paper fallback in `make_engine` arms the operator HALT sentinel.
    ///
    /// A mounted paper book is a MOUNT — it is what a venue with no credentials falls back to under
    /// the live gate — so `touch <project>/settings/state/HALT` has to reach it. Before `paper_client` existed the
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

/// The PRE-CONNECT budget preview (the #817 "refusal happens POST-connect" residual). The probe's
/// roster tests — each venue's credential shapes through its bridge's `resolve`, and the arming
/// ceiling over them — run where the registry is: `crates/vike-tradehub/tests/mount_roster.rs`'s
/// `preconnect` module (docs/decisions/0096).
#[cfg(test)]
mod preconnect_tests;

#[cfg(test)]
mod account_event_lane_tests;

/// The generic fold over a `VenueRow::Mount` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no bridge implementing the contract yet (docs/decisions/0096).
#[cfg(test)]
mod contract_tests;
