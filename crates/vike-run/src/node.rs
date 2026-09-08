//! `node.rs` â PR-8 of the two-layer plan (Layer 2: extract the node assembly).
//!
//! [`build_node`] is the twelve-venue mount assembly, **moved verbatim from vike-app/main.rs**; the
//! daemon (PR-9) and the GUI both call build_node. It stands up the exact same live core a GUI-less
//! caller needs: every venue's [`vike_exec::ExecutionEngine`] built through [`vike_mount::make_engine`]
//! (so this crate names NO bridge crate directly â it goes THROUGH vike-mount, per the down-only
//! layering), the `recon_clients` list, the single-writer core ([`vike_core::spawn_core_multi`]) with a
//! caller-built [`vike_core::CoreConfig`], and the live-event forwarder (the standalone event lane every
//! live venue's exec client pushes into, relayed into the core ingest right after spawn).
//!
//! ## The boundary â what stays with the GUI (and why the daemon doesn't miss it)
//! build_node deliberately does NOT create the market-data feeds, the GUI `BookStore`/`TradeStore`, the
//! `CoreSinkAdapter`, or the [`vike_core::ReconDriver`]. Those are GUI-coupled and stay in `vike-app`:
//! the feeds are built over the composed sink (which carries L2 depth to the GUI book store and repaints
//! the egui `Context`) and name the bridge crates DIRECTLY (`vike_binance::market_feed::Feeds`, â¦), which
//! this crate must not. The `ReconDriver` mount reads the per-venue feed-status health map
//! (`recon_feed_statuses`), which only exists once those feeds are built â so it too stays next to the
//! feeds, and the caller mounts it from [`Node::recon_clients`] + [`Node::recon_trigger`] this returns.
//! A headless caller with no feeds simply mounts recon with an empty status map (every venue reads
//! Healthy) or skips it â the exact `vike_run::build_paper_maker_core` + caller-owned-feed shape the
//! paper maker mount already uses.
//!
//! ## Inputs/outputs
//! Everything the `make_engine` block reads is a [`NodeConfig`] field (the credentials `.env` map, the
//! optional PIT-properties recorder, the per-venue seed cash, the reconcile master gate, the machine's
//! [`vike_mount::MountPolicy`] ceilings, and the
//! fully-built `CoreConfig`); everything a caller wires the rest of the process from is a [`Node`]
//! field (the [`vike_core::CoreHandle`], the recon ingredients, the live-venue set, the forwarder
//! stop flag).
//!
//! ## The startup preflight
//! [`build_node`] runs [`vike_mount::startup::run_startup_preflight`] before the first `make_engine`
//! call and logs its report â the GUI and the daemon share this site, so neither has to remember to
//! run it. The report is ADVISORY (see [`build_node_with_preflight`], the injected-report twin the
//! tests drive): a preflight failure logs loudly and the mount proceeds. It deliberately carries no
//! [`NodeConfig`]/[`Node`] field â both are constructed and destructured with exhaustive struct
//! literals in `vike-app`, so growing either would be a breaking change for a caller this wiring has
//! no business touching.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_core::{CoreConfig, CoreHandle};
use vike_mount::preflight::{CheckStatus, PREFLIGHT_SKIP_ENV, PreflightReport};

/// The INPUTS the twelve-venue assembly reads. Consumed BY VALUE (see [`build_node`]) because it owns
/// the move-only [`CoreConfig`] (which holds `Box<dyn FnMut>` closures) and the reconnect-trigger
/// receiver â neither is `Clone`, so a `&NodeConfig` could not hand them to `spawn_core_multi`/
/// `spawn_recon`.
pub struct NodeConfig {
    /// The workspace `.env` credentials map (`vike_bridge_core::credentials::load_workspace_dotenv()`),
    /// threaded into every `make_engine` call. Absent credentials ARE the live gate, so an empty map â
    /// every venue mounts the PAPER fallback (proven by `build_node_paper.rs`).
    pub vars: HashMap<String, String>,
    /// The opt-in PIT-`SymbolProperties` recorder (`VIKE_RECORD_PROPERTIES=1`), CONSTRUCTED by the
    /// binary â `vike_data::PropertiesRecorder::open_from_env` names the concrete DataFusion store
    /// backend, which only the binaries carry (vike-app's `fat`, vike-tradehub's
    /// `record-feeds`/`materialize`); this crate and vike-mount stay trait-only over vike-data.
    /// Cloned into every `make_engine` call below, so the five recording arms share ONE store
    /// handle. `None` â a binary without the backend, or the env gate unset (the default) â is
    /// byte-identical to no recorder at all.
    pub properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// Per-venue account seed (equity) for the `extra` engines. The PRIMARY engine's seed rides
    /// `core_config.seed_cash` instead; the GUI mount passes `10_000.0` for both.
    pub seed_cash: f64,
    /// The reconcile master gate (`VIKE_RECONCILE=1`, resolved by
    /// `vike_ops::reconcile_config::reconcile_enabled` over the REAL process env). Drives whether
    /// the shared reconnect-trigger channel is built and cloned into every live venue's resync
    /// supervisor. The caller reuses the SAME value for its `ReconDriver` mount.
    pub recon_enabled: bool,
    /// The fully-built [`CoreConfig`] â `seed_cash` plus every safety/observer knob (repaint hook,
    /// counters mirror, equity sampler, journal, drawdown latch, stuck-order watchdog, â¦). Built by the
    /// caller (the GUI-specific knobs stay there) and CONSUMED at `spawn_core_multi`.
    pub core_config: CoreConfig,
    /// The operator's `[risk]` budget (RunProfile wiring â closing the live gap), threaded VERBATIM
    /// into every one of the twelve `make_engine` calls below â the SAME profile a caller resolves
    /// once (typically via `vike_core::resolve_profile`) applies uniformly across every venue this
    /// node mounts, exactly like `recon_enabled`/`vars` above. `None` â every caller before this field
    /// existed, and any caller with no operator profile configured â leaves every venue's
    /// `RiskLimits` exactly as `make_engine` built it pre-wiring (BYTE-IDENTICAL): only the venue grid
    /// and the `im_requirement` rescue are armed. `Some` merges the SAME `[risk]` table a backtest or
    /// paper mount of the same strategy would read, via `vike_exec::ProfileRisk::apply_to` â see
    /// `vike_mount::make_engine`'s doc for the merge rule and its config-error handling.
    pub risk_profile: Option<vike_exec::ProfileRisk>,
    /// This MACHINE's hard ceilings â the deployment's `<vike home>/policy.toml`, loaded by the
    /// BINARY (`vike_config::load` takes the environment as a MAP; only binaries read `std::env`)
    /// and projected onto [`vike_mount::MountPolicy`], the subset a venue mount applies. Threaded
    /// VERBATIM into all twelve `make_engine` calls below, exactly like `risk_profile`/`vars`/
    /// `recon_enabled`, so one machine's ceilings apply uniformly across every venue this node
    /// mounts.
    ///
    /// A DIFFERENT authority from [`Self::risk_profile`]: a run profile is per-RUN and
    /// operator-owned (file, env, CLI); a policy is per-MACHINE and admin-owned with **no env or
    /// CLI layer at all** â that reduction is the entire reason `vike_config::Policy` is its own
    /// type.
    ///
    /// [`vike_mount::MountPolicy::default()`] is exactly "no `policy.toml` on this machine". â  That
    /// is NO LONGER byte-identical to a mount before this field existed, and the exception is
    /// `MountPolicy::venues` â the per-venue ARMING CEILING, whose default is `paper` for EVERY
    /// venue. A node built with the default policy therefore mounts every venue paper regardless of
    /// what its credentials say, which is the point: credential presence was the only gate, and a
    /// ceiling whose absent value armed everything would not be a ceiling. Every other field still
    /// leaves each venue on its own compiled-in literal.
    ///
    /// `vike_mount::make_engine`'s doc carries the fold, and `vike_mount::venue_arming_migration`
    /// is the once-per-process warning a credentialled box with no `[venues]` table gets.
    pub policy: vike_mount::MountPolicy,
}

/// The spawned node: the live [`CoreHandle`] plus the handles a caller wires the rest of the process
/// from. Destructure it into same-named locals (`let Node { handle: core, recon_clients, recon_trigger,
/// live_venues, forwarder_stop } = node;`) so downstream code reads exactly as it did before the move.
pub struct Node {
    /// The single-writer live core (the `vt-core` runtime).
    pub handle: CoreHandle,
    /// One `(venue, ReconClient)` per credentialed venue with a wired reconcile client â EMPTY on a
    /// pure-paper mount (`filter_map` drops the `None`s). The caller mounts `vike_core::spawn_recon`
    /// from this (it also needs the per-venue feed-status health map, which lives with the feeds).
    pub recon_clients: Vec<(String, Box<dyn vike_exec::recon::ReconClient>)>,
    /// The pre-built reconnect-trigger channel (`Some` iff [`NodeConfig::recon_enabled`]): its `Sender`
    /// was cloned into every live binance/bybit/okx/hyperliquid resync supervisor, and the `(tx, rx)`
    /// pair is handed to `vike_core::spawn_recon` so a reconnect poke reaches the SAME driver
    /// (`spawn_recon` ADOPTS the pair). `None` when reconcile is off â byte-identical to a build with no
    /// channel.
    pub recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    /// Venues with a credential-gated LIVE exec client (else paper) â the set the DOM lights its
    /// `â LIVE` badge from and the caller warns about.
    pub live_venues: HashSet<String>,
    /// Raised BEFORE core shutdown so the live-event forwarder drains-and-drops (no teardown deadlock â
    /// see the forwarder's teardown-safety note below).
    pub forwarder_stop: Arc<AtomicBool>,
}

/// [`build_node`] failure. The assembly had exactly one fault before Task 6 (armed-risk-defaults)
/// â the live-event forwarder THREAD spawn. Task 6 adds a second, deliberate fault: a LIVE venue
/// (one of the twelve `make_engine` arms below actually resolved credentials) that refuses to
/// start because the operator supplied no account-dependent risk budget
/// (`max_notional_per_order`/`max_total_exposure` â see [`vike_mount::require_live_risk_budget`]'s
/// doc for why no universal default exists for either). Kept a real error type so the signature
/// does not churn when a future fallible mount step (e.g. a store open) lands.
///
/// **`Display` is the operator-facing contract here, not a label.** Both callers PRINT it and exit
/// (vike-app's `App::new`, vike-tradehub's `VIKE_TRADEHUB_LIVE=1` arm) â neither `.expect`s it any
/// more, because the risk-budget arm is a configuration mistake a new user hits on their FIRST run
/// and a `Debug`-formatted panic told them nothing they could act on. The `RiskBudget` arm below
/// therefore forwards [`vike_mount::MountError`]'s `Display` VERBATIM (no prefix, no summary): that
/// impl carries the whole diagnostic â the resolver to set, the missing keys, and a working
/// `[risk]` example â and wrapping it would corrupt the TOML block it prints.
/// `vike-run/tests/risk_budget_diagnostic.rs` pins the verbatim forwarding.
#[derive(Debug)]
pub enum NodeError {
    /// The `live-event-forward` OS thread could not be spawned.
    ForwarderSpawn(std::io::Error),
    /// A live venue mount refused to start over a missing account-dependent risk budget.
    RiskBudget(vike_mount::MountError),
    /// **A venue armed live that the pre-mount armed set did not name** â the
    /// [`refuse_unarmed_live_venues`] backstop. It means [`armed_live_venues`]'s hand-written
    /// probe UNDER-counted (a new live arm without a probe row is the shape its own doc predicts),
    /// so the composition root claimed no `vike_ops::live_lock` sentinel for that venue's account
    /// while the mount opened a real exec session on it. Refusing the node is the safe end: an
    /// unlocked live venue is exactly the Danger-2 accident the lock exists to make impossible.
    UnarmedLiveVenues {
        /// The venues that armed live outside the armed set, sorted.
        venues: Vec<String>,
        /// The pre-mount armed set the caller locked from, for the diagnostic.
        armed: Vec<String>,
    },
    /// **A strategy mount named an account this box will not arm.** The one refusal in this crate
    /// that is not a degrade, and `vike_run::refuse_unarmed_mount_accounts` carries the argument:
    /// a mount silently running on a venue's DEFAULT account when its author named another trades
    /// the wrong book with no error, so there is no degraded state that is safe. The message names
    /// the venue, the account and the mount's own symbol.
    Mount(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::ForwarderSpawn(e) => write!(f, "spawn live-event-forward thread: {e}"),
            NodeError::RiskBudget(e) => write!(f, "{e}"),
            NodeError::UnarmedLiveVenues { venues, armed } => write!(
                f,
                "refusing the node: {venues:?} armed a LIVE exec client, but the pre-mount armed \
                 set was {armed:?} â so no live-account lock was claimed for {venues:?} and a \
                 second live process on those accounts would not be refused. This is a probe \
                 UNDER-COUNT: `vike_mount::would_mount_live_under` carries no row matching the \
                 arm that fired. Add the row (it lives beside the arm's own credential loader), \
                 or remove the arm."
            ),
            NodeError::Mount(m) => write!(f, "refusing the node: {m}"),
        }
    }
}

impl std::error::Error for NodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NodeError::ForwarderSpawn(e) => Some(e),
            NodeError::RiskBudget(e) => Some(e),
            // No inner error to forward â the backstop's whole diagnostic is the two sets, and
            // `Display` above carries them.
            NodeError::UnarmedLiveVenues { .. } | NodeError::Mount(_) => None,
        }
    }
}

// The per-venue wired-market rows [`WIRED_MARKETS`] is assembled from. Private on purpose â the
// TABLE is the public surface; each row exists so [`build_node`]'s `make_engine` call and the
// table share ONE definition (the row a call reads IS the row the table lists, so they cannot
// drift).
// vike:new-venue:note do NOT add a `{VENUE}_MARKET` row or a `WIRED_MARKETS` entry for `{venue}` yet. A row here is what ARMS the venue in this node, and it is only correct once `vike_mount::make_engine` has a real arm for it — the same rule, for the same reason, as the note in `crates/vike-mount/src/lib.rs`. When it IS time: the symbol must be a pair that venue's own live smoke actually drives (a guessed ticker mounts a feed nobody watches), it needs a matching entry in `vike-tradehub`'s `DaemonProfile::validate_for_live` allow-list in the SAME PR, and if the arm is feature-gated the row takes the same `#[cfg]` so the table equals what this build wires.
const BINANCE_MARKET: (&str, &str) = ("binance", "BTCUSDT");
const BYBIT_MARKET: (&str, &str) = ("bybit", "BTCUSDT");
const OKX_MARKET: (&str, &str) = ("okx", "BTC-USDT-SWAP");
const DERIBIT_MARKET: (&str, &str) = ("deribit", "BTC-PERPETUAL");
const HYPERLIQUID_MARKET: (&str, &str) = ("hyperliquid", "BTC");
const ASTER_MARKET: (&str, &str) = ("aster", "BTCUSDT.P");
const ALPACA_MARKET: (&str, &str) = ("alpaca", "AAPL");
const CTRADER_MARKET: (&str, &str) = ("ctrader", "EURUSD");
const IG_MARKET: (&str, &str) = ("ig", "CS.D.EURUSD.MINI.IP");
const OANDA_MARKET: (&str, &str) = ("oanda", "EURUSD");
#[cfg(feature = "ibkr")]
const IBKR_MARKET: (&str, &str) = ("ibkr", "AAPL.SMART.USD");
/// FXCM ForexConnect, behind this crate's `fxcm` feature. EUR/USD because it is the pair the demo
/// account trades and the one both live smokes drive; `crates/bridges/fxcm/src/exec.rs`'s
/// `to_fxcm_instrument` maps it to the venue's slashed `EUR/USD`.
#[cfg(feature = "fxcm")]
const FXCM_MARKET: (&str, &str) = ("fxcm", "EURUSD");
/// Polymarket's symbol is EMPTY on purpose â its mount is ACCOUNT-WIDE (exec, the user-WS fill
/// pump and reconcile all key off the wallet, not a market); see the arm's comment in
/// [`build_node`] for why any literal token here would be stale within the minute.
#[cfg(feature = "polymarket")]
const POLYMARKET_MARKET: (&str, &str) = ("polymarket", "");

/// The hardcoded `(venue, symbol)` pairs [`build_node`] mounts â THE wired-market table (audit F5).
///
/// [`build_node`]'s `make_engine` calls read their venue+symbol FROM these same rows (each row is
/// one shared `const`), so this table cannot drift from the assembly. It exists so the OTHER place
/// that names live-wired pairs â `vike-tradehub`'s `DaemonProfile::validate_for_live` allow-list â
/// can be completeness-tested against it (allow-list â this set, the CLAUDE.md capability-map
/// STEP-1 pattern). Extending the daemon to another venue therefore REMAINS a deliberate two-place
/// edit (a row here consumed by a `build_node` arm, plus the daemon's allow-list), never a silent
/// widening â the sync test in `vike-tradehub` is what makes forgetting one place loud.
///
/// The ibkr/polymarket/fxcm rows exist only under their cargo features, exactly like the
/// `make_engine` arms they feed â so the table always equals what THIS build actually wires.
pub const WIRED_MARKETS: &[(&str, &str)] = &[
    BINANCE_MARKET,
    BYBIT_MARKET,
    OKX_MARKET,
    DERIBIT_MARKET,
    HYPERLIQUID_MARKET,
    ASTER_MARKET,
    ALPACA_MARKET,
    CTRADER_MARKET,
    IG_MARKET,
    OANDA_MARKET,
    #[cfg(feature = "ibkr")]
    IBKR_MARKET,
    #[cfg(feature = "polymarket")]
    POLYMARKET_MARKET,
    #[cfg(feature = "fxcm")]
    FXCM_MARKET,
];

/// **The venues this node will actually ARM live â computed BEFORE anything is constructed.**
///
/// One row per [`WIRED_MARKETS`] venue whose [`vike_mount::would_mount_live_under`] probe answers
/// `true` under that venue's own arming ceiling ([`vike_mount::MountPolicy::venue_mode`]) â i.e.
/// exactly the question [`build_node`]'s mount arms are about to ask, asked from a PURE function
/// that opens no socket, signs nothing and builds no client.
///
/// # Why this exists, and what it is FOR
///
/// The B11 live-account lock (`vike_ops::live_lock`) claims one sentinel per venue ACCOUNT, and a
/// claim is only worth anything if it covers what is actually live and is held BEFORE the first
/// exec client exists. Neither composition root could compute that set on its own:
///
/// * `vike-tradehub` locked its RUN PROFILE's mount set, which answers a different question
///   entirely â the profile decides which `(venue, symbol)` pairs carry a STRATEGY, while this
///   node arms an exec client for every wired venue the credential store answers for. MEASURED on
///   the CI box, from one startup of the shipped daemon, two lines apart:
///   `live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}`
///   beside `{"kind":"ready","mode":"LIVE (venue=bybit)"}` â nine live authenticated sessions, one
///   lock. A second process with a different profile locked a venue the first never locked, saw no
///   conflict, and both traded one account.
/// * `vike-app` locked the RIGHT set ([`Node::live_venues`]) at the WRONG TIME: that set does not
///   exist until [`build_node`] has already built every exec client, and three of those arms
///   (`crates/bridges/bybit/src/exec.rs`, `crates/bridges/okx/src/exec.rs` and
///   `crates/bridges/aster/src/exec.rs` â each posts `set_leverage` at startup) make a
///   post-construction refusal no longer side-effect-free.
///
/// # Direction of error, stated because it is not symmetric
///
/// The probe is INTENT-based (its own doc says so), so this set can be a strict SUPERSET of the
/// [`Node::live_venues`] the mount ends up recording â a venue whose synchronous connect fails
/// (ctrader/ibkr) or whose factory declines a present-but-bad key (hyperliquid/polymarket) probes
/// live and mounts paper. That direction costs one extra sentinel file and one venue on which a
/// SECOND process is refused; the other direction â a venue that arms with nothing holding its
/// lock â is the one this whole seam exists to prevent, and [`refuse_unarmed_live_venues`] is the
/// runtime backstop for it.
/// # â  It answers ROUTE KEYS, one per ACCOUNT â not venue ids
///
/// The unit the lock protects is a venue ACCOUNT (`vike_ops::live_lock`'s own module doc says so),
/// and since the mount fans out per account there can be more than one of those per venue. A route
/// key IS the account's identity â `vike_exec::ExecutionEngine::route_key`, the name
/// `make_engine_accounts` records in `live_venues`, and the `LIVE-<route_key>.lock` filename â so
/// the set claimed, the set recorded and the set compared are one vocabulary rather than three.
///
/// For a venue's DEFAULT account the route key IS the bare venue id, so a box with no `[accounts]`
/// table produces exactly the `Vec` this function produced before (as `String`s), claims exactly the
/// sentinel filenames it always claimed, and moves no deployment's lock.
///
/// It goes through `vike_mount::venue_account_arming` rather than through
/// `vike_mount::would_mount_live_under` â the finer function the coarse one is derived from â because
/// only the finer one can enumerate a venue's ACCOUNTS (the coarse one answers for the default one). The
/// answer for a single-account venue is identical by construction: `would_mount_live_under` is
/// `venue_arming_under(..).0 != Paper`, which is the same comparison made below.
///
/// # ⚠ It does NOT depend on the strategy mount set, and that is worth stating
///
/// A labelled account ARMS on its `policy.accounts.<venue>.<LABEL>` line plus its own credentials,
/// and `vike_mount::make_engine_accounts` mounts every active account whether or not a strategy
/// names it. So the set of live accounts — and therefore the set of sentinels to claim — is a fact
/// about the SETTINGS alone. What a mount adds is which account a strategy TRADES on, and that is a
/// different question, answered in `vike_core` and refused here by
/// [`refuse_unarmed_mount_accounts`].
#[must_use]
pub fn armed_live_venues(
    vars: &HashMap<String, String>,
    policy: &vike_mount::MountPolicy,
) -> Vec<String> {
    WIRED_MARKETS
        .iter()
        .flat_map(|(venue, _)| vike_mount::venue_account_arming(venue, vars, Some(&policy.venues)))
        .filter(|row| row.effective != vike_mount::VenueMode::Paper)
        .map(|row| row.route_key())
        .collect()
}

/// **One strategy mount's ROUTING declaration, as strings** — the minimum
/// [`account_symbols_for`] needs, and the whole of what a mount contributes to the arming question.
///
/// It exists as its own type rather than as a `(String, String, Option<AccountLabel>)` because
/// three same-typed fields in a tuple say nothing about which slot is which, and this one is read
/// by [`armed_live_venues`] — the function whose answer decides which live-account LOCKS are
/// claimed. Built from a [`vike_core::CoreConfig`]'s mounts by [`mount_accounts`], and from a
/// composition root's own specs before the core exists (which is why it is not simply derived: the
/// tradehub claims its locks before it has a `CoreConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountAccount {
    /// The mount's venue — a `vike_model::VENUES` id.
    pub venue: String,
    /// The instrument this mount trades, which is the symbol its ACCOUNT is armed on.
    pub symbol: String,
    /// Which account of that venue, `None` for the venue's default one.
    pub account: Option<vike_model::account_keys::AccountLabel>,
}

/// The [`MountAccount`] rows a built [`CoreConfig`] declares — the same list a composition root
/// computed from its own specs, read back off the config so [`build_node`]'s own
/// [`armed_live_venues`] call cannot consult a different mount set from the one it is about to
/// assemble.
#[must_use]
pub fn mount_accounts(core: &CoreConfig) -> Vec<MountAccount> {
    core.strategy
        .iter()
        .chain(core.extra_mounts.iter())
        .map(|m| MountAccount {
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
            account: m.account.clone(),
        })
        .collect()
}

/// **WHICH SYMBOL each account of `venue` is armed on** — the map that replaced the single symbol
/// every account of a venue used to share, and the half of "a strategy names its account" that
/// makes a labelled account's ENGINE mount on the instrument its own strategy trades.
///
/// ⚠ Two accounts may perfectly well land on the SAME symbol here. That is the spread the deleted
/// collision rule refused (`vike_config::venue_accounts`), and nothing about this map is a
/// uniqueness claim.
///
/// The rule, one line per case:
///
/// * the DEFAULT account is ALWAYS present and ALWAYS `wired_symbol` — unconditionally, even if a
///   mount names it on another instrument. [`WIRED_MARKETS`] is what [`build_node`] mounts that
///   venue's primary engine on, and `vike_tradehub::config::DaemonProfile::validate_for_live`
///   already refuses an account-less live row that names anything else;
/// * one entry per DISTINCT labelled account named by a mount on this venue, its symbol being that
///   mount's own `symbol`. That is the whole mechanism: the mount decides the instrument and the
///   mount names the account, so the account's symbol is the mount's symbol and no settings key has
///   to restate it;
/// * a labelled account NO mount named contributes no entry, so `vike_mount::symbol_for_account`
///   falls it back to the wired symbol — an account somebody armed in `policy.toml` and pointed at
///   no mount is reachable INBOUND on that symbol and addressed by no strategy, which is exactly
///   what arming an account without mounting anything on it means;
/// * mounts on OTHER venues contribute nothing.
///
/// Order: `Default` first, then labelled accounts in DECLARATION order. The default-first half is
/// load-bearing — `vike_mount::make_engine_accounts` returns the default account's engine at `[0]`
/// and every caller binds it to the engine it has always bound.
///
/// ⚠ **Two mounts naming ONE labelled account on TWO symbols is a LOAD refusal**, not a pick made
/// here: `vike_tradehub::config::DaemonProfile`'s multi-mount validation names both rows. This
/// function takes the FIRST such mount's symbol so that it is total, but nothing that reaches a
/// live mount can carry the ambiguity.
#[must_use]
pub fn account_symbols_for(
    mounts: &[MountAccount],
    venue: &str,
    wired_symbol: &str,
) -> Vec<(vike_model::account_keys::AccountLabel, String)> {
    use vike_model::account_keys::AccountLabel;
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

/// **THE LOUD REFUSAL: a mount naming an account this box will not ARM must not start.**
///
/// Every other refusal in this workspace degrades — a venue that cannot arm falls back to paper and
/// the daemon comes up (`docs/decisions/0013-degrade-vs-refuse.md`). This one does not, and the
/// asymmetry is the whole point of the `account` field: a strategy silently running on the DEFAULT
/// account when its author named another trades the wrong book with no error anywhere. There is no
/// degraded state that is safe, because "degraded" here means "trading, on someone else's money".
///
/// It is checked against the SAME `vike_mount::venue_account_arming` the fan-out selects with, over
/// the same vars and the same policy, so it cannot refuse a mount the fan-out would have armed or
/// pass one it would not. `vike_core`'s `mount_engine_idx` panics on the same miss — that is the
/// backstop no future composition root can reach past; this is the message an operator can act on.
///
/// A mount naming NO account is never refused here: it resolves the venue's default engine exactly
/// as it always has, and [`build_node`] mounts that engine for every wired venue regardless of
/// arming.
fn refuse_unarmed_mount_accounts(
    cfg: &NodeConfig,
    mounts: &[MountAccount],
) -> Result<(), NodeError> {
    for m in mounts {
        let Some(label) = m.account.as_ref().filter(|l| !l.is_default()) else { continue };
        let armed = vike_mount::venue_account_arming(&m.venue, &cfg.vars, Some(&cfg.policy.venues))
            .into_iter()
            .any(|row| &row.label == label && row.effective != vike_mount::VenueMode::Paper);
        if !armed {
            return Err(NodeError::Mount(format!(
                "strategy mount on {}/{} names account `{label}`, which this box will not arm — so \
                 no `{}#{label}` engine exists and the mount would otherwise run on {}'s DEFAULT \
                 account, trading a book its author did not choose. Arm it: \
                 `policy.accounts.{}.{label}` in <project>/settings/policy.toml, plus that \
                 account's own `__{label}` credential keys (there is NO fallback to the unlabelled \
                 keys). Or drop `account` from the mount.",
                m.venue, m.symbol, m.venue, m.venue, m.venue
            )));
        }
    }
    Ok(())
}

/// **THE BACKSTOP: nothing may arm live outside the set that was locked.**
///
/// [`armed_live_venues`] is a hand-written probe â `vike_mount::would_mount_live_under`'s own doc
/// admits that "a NEW live arm added without a row here is still caught by the post-merge
/// backstop", and that backstop only ever checked the RISK BUDGET. For a LOCK, under-counting is
/// the dangerous direction: the venue arms, places real orders, and no sentinel is held for its
/// account â which is precisely the accident the lock exists to refuse, wearing the appearance of
/// a correct startup.
///
/// So [`build_node`] compares the two sets it now has â the pre-mount armed set the caller locked
/// from, and the post-mount `live_venues` record `vike_mount::make_engine_with_legs` wrote â and
/// refuses the whole node when the record is not a SUBSET. A silently-unlocked live venue becomes
/// a startup refusal instead.
///
/// The comparison is deliberately one-directional: `armed â live_venues` is the accepted
/// over-count [`armed_live_venues`] documents, and refusing it would fail an ordinary
/// bad-key/failed-connect startup.
///
/// â  **Both sides are ROUTE KEYS now** â see [`armed_live_venues`]. That is what keeps the
/// comparison meaningful once a venue can have two accounts: an armed `binance#ALT` that the probe
/// did not name would otherwise be masked by a `binance` sitting in both sets, and the account with
/// no lock behind it is exactly what this refusal exists to catch.
pub fn refuse_unarmed_live_venues(
    armed: &[String],
    live_venues: &HashSet<String>,
) -> Result<(), NodeError> {
    let mut unarmed: Vec<String> =
        live_venues.iter().filter(|v| !armed.contains(v)).cloned().collect();
    if unarmed.is_empty() {
        return Ok(());
    }
    unarmed.sort();
    Err(NodeError::UnarmedLiveVenues { venues: unarmed, armed: armed.to_vec() })
}

/// The `block` for a venue the pre-mount probe said would arm and that then did NOT appear in
/// [`Node::live_venues`].
///
/// No [`vike_config::ArmingBlock`] variant can carry this and none should: every variant there is
/// answerable BEFORE the network â a missing credential, an unset flag, an absent build feature.
/// This one is only knowable AFTER, and it is the single most useful line this channel can write:
/// the operator armed a venue, the ceiling permitted it, the credentials were present, and it
/// still traded paper because its connect failed. Nothing in the tree records that today.
///
/// It reaches the record as a STRING precisely because
/// [`vike_model::change_journal::VenueMountTarget::block`] takes one â the layer split that forced
/// it also bought this.
pub const MOUNT_FAILED: &str = "mount failed";

/// Journal what each venue was ASKED to be and what it BECAME â one `venue_mounted` record per
/// venue that is interesting, written once per process start.
///
/// **The comparison is the point, and it needs BOTH sides.** [`vike_mount::venue_arming`] is a
/// PREDICTION: pure, network-free, and therefore exactly what the Data Manager's arming screen
/// renders. [`Node::live_venues`] is the OUTCOME. A record built from the predicate alone would
/// only restate the screen; a record built from the outcome alone could not say what was asked
/// for. Written together they answer the question the per-venue switch invites â *"I set live, why
/// did it trade paper?"* â including the one answer no pre-mount reading can give
/// ([`MOUNT_FAILED`]).
///
/// **Which venues are recorded:** those whose ceiling is above `paper`, plus any that actually
/// armed. A venue sitting at the DEFAULT `paper` ceiling that duly mounted paper is not news â it
/// is the documented default for a box with no `[venues]` table, and `vike_boot`'s `boot_settings`
/// anchor already brackets the ceilings. â  This is not the "record on agreement too" rule being
/// walked back: a `live` venue that reaches `live` still gets a line. What is exempt is the
/// all-paper DEFAULT case, and only because something else already asserts it.
///
/// `state_dir` `None` â no project above the working directory â writes NOTHING rather than
/// inventing a ledger location, the same rule `vike_boot::journal_boot_settings` follows and the
/// same one `vike-tradehub`'s `a_journal_less_surface_writes_nothing` pins.
///
/// `ts_ms` is a PARAMETER: `vike_model::change_journal` reads no clock, so a composition root
/// stamps it. Returns one result per record ATTEMPTED, so a caller can log the failures without
/// this function taking a `tracing` opinion.
///
/// â  **It takes the ROWS, not the vars map, and that is a correctness property rather than an
/// ergonomic one.** `vike-tradehub`'s `live_mount_with` STRIPS a `data_only` venue's exec
/// credentials from its map before mounting, so there are two different maps in that function and
/// only one of them gives the right answer. A signature taking `vars` lets a caller hand over the
/// wrong one and be told so by a comment; a signature taking `&[VenueArming]` makes the caller
/// compute them â through [`vike_mount::venue_arming`] â at a moment it has to choose deliberately.
/// The daemon computes them before its map is moved into the node config, which is after the
/// withhold; nothing else is reachable.
pub fn journal_venue_mounts(
    state_dir: Option<&std::path::Path>,
    arming: &[vike_mount::VenueArming],
    live_venues: &HashSet<String>,
    version: &str,
    ts_ms: i64,
) -> Vec<Result<std::path::PathBuf, vike_model::change_journal::ChangeJournalError>> {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};
    use vike_mount::VenueMode;

    let Some(state_dir) = state_dir else { return Vec::new() };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(version));
    arming
        .iter()
        .filter(|row| row.ceiling != VenueMode::Paper || live_venues.contains(&row.route_key()))
        .map(|row| {
            // â  The membership test is the ROUTE KEY, not the venue: `live_venues` is keyed per
            // ACCOUNT now, so asking it about `binance` would answer for the DEFAULT account and
            // record `binance#ALT` as having failed to mount whenever the default one armed.
            let armed = live_venues.contains(&row.route_key());
            // The OUTCOME. `live_venues` carries names, not tiers, so a venue that armed is
            // recorded at the tier the probe predicted for it â the probe and the mount consult one
            // row set (`venue_arming_under`), so they cannot disagree about WHICH tier; they can
            // only disagree about WHETHER, which is what `armed` settles.
            let effective = if armed { row.effective } else { VenueMode::Paper };
            let block = if effective == row.ceiling {
                None
            } else if !armed && row.effective != VenueMode::Paper {
                Some(MOUNT_FAILED)
            } else {
                Some(row.block.as_str())
            };
            journal.append(
                ts_ms,
                &Change::venue_mounted(
                    Outcome::Applied,
                    Actor::Boot,
                    row.venue,
                    // `None` for the DEFAULT account, which SKIPS the field â so a box with one
                    // account per venue writes the record it has always written, byte for byte.
                    row.label.text(),
                    row.ceiling.as_str(),
                    effective.as_str(),
                    block,
                ),
            )
        })
        .collect()
}

/// The EXTRA symbols this node's mounts declared for `venue`, beyond the ONE symbol
/// [`WIRED_MARKETS`] wires that venue's engine on â i.e. the `declared_legs` argument of
/// `vike_mount::make_engine_with_legs`, which is the ONLY producer of
/// `vike_exec::RiskLimits::grid_by_symbol` in production.
///
/// **This is where the multi-symbol fact actually lives.** `build_node` hardcodes one
/// `(venue, symbol)` pair per venue, so the engine alone cannot know that a strategy also trades a
/// second instrument on it â only a `vike_core::StrategyMount`'s `symbols` (`vike_core::MountLeg`)
/// says so, and that declaration is already in `NodeConfig::core_config` by the time the venue arms
/// run (`vike_run::build_live_xemm_core` and `crates/vike-run/src/lib.rs`'s `build_live_maker_core` both assign
/// `core_config.strategy` BEFORE calling `build_node`). Without this the grid map could only ever
/// be populated by a caller that does not exist.
///
/// EMPTY for every mount in this workspace today: `crates/vike-run/src/lib.rs`'s `MountSpec`
/// carries `legs` and every spec this crate builds leaves it empty, and the live
/// xEMM mount's one `MountLeg::at` can only name the taker venue's own wired symbol (anything else
/// is refused up front as `XemmConfigError::HedgeSymbolNotAccepted`, because `make_engine` sets no
/// `extra_symbols` and that engine would drop the hedge). That is exactly why this wiring is inert
/// on arrival: an empty slice makes `make_engine_with_legs` touch nothing at all.
fn declared_legs_for(core: &CoreConfig, venue: &str, wired_symbol: &str) -> Vec<String> {
    legs_for_venue(
        core.strategy
            .iter()
            .chain(core.extra_mounts.iter())
            .map(|m| (m.venue.as_str(), m.symbols.as_slice())),
        venue,
        wired_symbol,
    )
}

/// [`declared_legs_for`]'s pure core, over the minimum each mount contributes â its own venue plus
/// its declared legs. Split out because a `StrategyMount` owns a `Box<dyn Strategy>` and cannot be
/// built in a unit test without a whole strategy, while the RULE below is entirely about strings
/// and is the part that can be wrong.
///
/// The rule, one line per hazard:
/// * A leg's venue is `vike_core::MountLeg::venue` when it names one (the cross-exchange
///   `MountLeg::at` shape) and the MOUNT's venue otherwise â the same resolution
///   `vike_core`'s `resolve_intent_venue` uses to route that leg's orders, so the engine that will
///   RECEIVE the order is the engine that gets its grid.
/// * The venue's own wired symbol is excluded: its grid is already the engine's scalars.
/// * Blanks are dropped and repeats collapse, so two mounts declaring the same hedge symbol cost
///   one lookup, not two (on a venue whose lookup is a network call that is a round trip saved).
/// * Order is DECLARATION order, and preserved: `grid_by_symbol` is an `IndexMap` (the repo-wide
///   IndexMap-not-HashMap rule), so a stable input keeps the serialized limits stable too.
fn legs_for_venue<'a>(
    mounts: impl Iterator<Item = (&'a str, &'a [vike_core::MountLeg])>,
    venue: &str,
    wired_symbol: &str,
) -> Vec<String> {
    let mut legs: Vec<String> = Vec::new();
    for (mount_venue, declared) in mounts {
        for leg in declared {
            let leg_venue = leg.venue.as_deref().unwrap_or(mount_venue);
            // â  The RAW symbol is what travels, and `trim()` is used only to DECIDE. `vike-core`'s
            // `resolve_intent_symbol` matches a declared leg with `l.symbol == r` and stamps the raw
            // declared string onto the `OrderRequest`, so a leg written with surrounding whitespace
            // routes under those exact bytes. Keying the grid on a trimmed spelling would file a row
            // `RiskLimits::grid_for` can never hit â silently reinstating the wrong-instrument
            // rounding this whole map exists to prevent, while carrying a row asserting the grid WAS
            // resolved. Two symbol-identity rules that must agree, so they do.
            let symbol = leg.symbol.as_str();
            if leg_venue != venue || symbol.trim().is_empty() || symbol == wired_symbol {
                continue;
            }
            if !legs.iter().any(|s| s == symbol) {
                legs.push(symbol.to_string());
            }
        }
    }
    legs
}

/// Build the twelve-venue live node. MOVED VERBATIM from `vike-app/src/main.rs`'s `App::new` â every
/// `vike_mount::make_engine` arm (all 12: binance/bybit/okx/deribit/hyperliquid/aster/alpaca/ctrader/
/// ig/oanda + the `#[cfg(feature = "ibkr")]` ibkr arm + the `#[cfg(feature = "polymarket")]` polymarket
/// arm), the `extra` assembly, the `recon_clients` list, `spawn_core_multi`, and the live-event
/// forwarder (the "dead-man" relay lane). The one non-move: the forwarder's `.expect(â¦)` became
/// `.map_err(NodeError::ForwarderSpawn)?` so the fault rides the return type. (Audit F5, a later
/// mechanical no-behavior change: each arm's `(venue, symbol)` literal pair moved into the shared
/// [`WIRED_MARKETS`] row consts above â same strings, one definition.)
///
/// STARTUP PREFLIGHT: this runs [`vike_mount::startup::run_startup_preflight`] FIRST â before any
/// venue is mounted â and hands the report to [`build_node_with_preflight`]. Mounting it here rather
/// than in `vike-app`'s `main.rs` is deliberate: `build_node` is shared by the GUI and the headless
/// daemon, so both get the same "before the first live order" gate from one site. With no
/// credentials it costs no network at all (see that fn's doc) â **and the same is true of a full
/// credential store under an all-`paper` arming ceiling**, which is what `cfg.policy` is threaded in
/// for; with credentials the ceiling ARMS, it is a bounded DNS round plus one clock read and one
/// signed balance read per armed, credentialed venue.
pub fn build_node(cfg: NodeConfig) -> Result<Node, NodeError> {
    // The disk leg watches what THIS mount will actually write: the journal directory the caller
    // already resolved into `CoreConfig`. Resolving it a second time here would be a competing
    // authority (the dir can come from a `RunProfile`'s `[sinks.journal]` or from
    // `VIKE_JOURNAL_DIR`, and only the caller knows which won), and a preflight that measures a
    // directory nothing writes to is worse than none. No journal â no disk row.
    let dirs: Vec<(String, std::path::PathBuf)> = cfg
        .core_config
        .journal
        .as_ref()
        .map(|j| vec![("journal".to_string(), j.dir.clone())])
        .unwrap_or_default();
    // â  THE ARMING CEILING reaches the preflight through this argument, and it is the same
    // `cfg.policy` every `make_engine` call below is handed. Without it the preflight derived its
    // work from credential PRESENCE alone and authenticated against venues this deployment had
    // capped to `paper` â see `vike_mount::startup`'s module doc.
    let report = vike_mount::startup::run_startup_preflight(&cfg.vars, &dirs, Some(&cfg.policy));
    build_node_with_preflight(cfg, &report)
}

/// [`build_node`] over an ALREADY-RUN preflight report â the injected-report twin, so the
/// "a preflight failure must never stop a mount" property is testable with a hard-failing report
/// and no network (`build_node` itself is this function over the REAL startup preflight).
///
/// The report is **ENFORCED**, per venue:
/// [`PreflightReport::venue_disposition`](vike_mount::preflight::PreflightReport::venue_disposition)
/// is `Paper` for a venue with a hard FAIL, and [`enforce_preflight`] makes that true of the mount
/// by withholding that venue's credentials before the arms run.
///
/// â  **It used to be advisory, and the reason it was is worth keeping.** "A per-venue FAIL is
/// raised by ANY authed-read error, a transient timeout included, so acting on it would let one
/// flaky startup second silently turn a live venue into a paper one." That was true and it was the
/// stated blocker: enforcing needed a confirmed-vs-transient distinction that did not exist. It
/// exists now â `vike_mount::preflight::CredentialGap` splits "the venue answered and refused"
/// (`Fail`, demotes) from "we never heard back" (`Warn`, never demotes), and
/// `vike_mount::startup::CREDENTIAL_PROBE_TIMEOUT` is the bound that separates them. So the
/// objection is answered rather than overruled, and the remaining argument is the one that settles
/// it: refused keys and ABSENT keys are the same fact, and absent keys already mount paper.
///
/// â  A GLOBAL fail (`go() == false` â no disk, no internet) still does NOT stop the mount. A daemon
/// that refuses to start crash-loops under `Restart=on-failure`, which is a worse failure than the
/// one being reported; it is logged at `error` and the assembly proceeds.
pub fn build_node_with_preflight(
    mut cfg: NodeConfig,
    preflight: &PreflightReport,
) -> Result<Node, NodeError> {
    log_preflight(preflight);
    enforce_preflight(preflight, &mut cfg.vars);
    build_node_inner(cfg)
}

/// Apply the report's per-venue dispositions to the credential map the arms are about to read.
///
/// One `withhold_venue_credentials` call per degraded venue: `make_engine` then sees absent
/// credentials and mounts that venue's paper fallback through the ordinary live gate, with no new
/// branch anywhere in the twelve arms. Every demotion is logged at `error` with the withheld count,
/// because a venue silently changing from live to paper is exactly the failure mode this whole lane
/// is arguing about â the operator must not have to infer it.
///
/// â  A venue whose keys were ALREADY absent yields `withheld == 0` and is logged as such: it was
/// never going to mount live, so the preflight changed nothing and must not claim it did.
fn enforce_preflight(report: &PreflightReport, vars: &mut HashMap<String, String>) {
    for venue in report.degraded_venues() {
        let withheld = vike_mount::startup::withhold_venue_credentials(vars, &venue);
        tracing::error!(
            "startup preflight DEGRADED {venue} to PAPER for this session ({withheld} credential \
             key(s) withheld) â a per-venue FAIL is confirmed evidence (the venue answered and \
             refused, or its clock is measurably out of band), and refused keys are the same fact \
             as absent ones; fix the cause in that venue's row above and restart, or set \
             {PREFLIGHT_SKIP_ENV}=1 to mount without any preflight at all"
        );
    }
}

/// Disclose the preflight report on the existing `tracing` path: one line per check (PASS at
/// `info`, WARN at `warn`, FAIL at `error` â a check that hard-failed is what an operator must see
/// in a terminal scroll), plus a summary line for a global no-go or a degraded venue. Never
/// returns anything the mount branches on.
///
/// â  [`CheckStatus::NotApplicable`] logs at `info`, NOT `warn`: it is a DECLARED property ("this
/// venue publishes no server clock, because â¦"), and the whole reason that status exists is that
/// the previous shape rendered it as a warning on every healthy mount, where it read as a fault and
/// taught operators to skim past the clock rows.
fn log_preflight(report: &PreflightReport) {
    if report.skipped {
        tracing::warn!(
            "startup preflight SKIPPED by {PREFLIGHT_SKIP_ENV}=1 â no clock, credential or network \
             check ran before this mount"
        );
        return;
    }
    for (check, line) in report.checks.iter().zip(report.lines()) {
        match check.status {
            CheckStatus::NotApplicable | CheckStatus::Pass => tracing::info!("preflight: {line}"),
            CheckStatus::Warn => tracing::warn!("preflight: {line}"),
            CheckStatus::Fail => tracing::error!("preflight: {line}"),
        }
    }
    if !report.go() {
        tracing::error!(
            "startup preflight is a NO-GO (a process-wide check hard-failed) â mounting anyway, \
             but expect venue connections to fail"
        );
    }
    let degraded = report.degraded_venues();
    if !degraded.is_empty() {
        tracing::error!(
            "startup preflight FAILED for {degraded:?} â those venues ARE demoted to paper for \
             this session (see the per-venue line each one logs next). A FAIL is confirmed \
             evidence: a probe that merely did not answer WARNs and changes nothing"
        );
    }
}

/// The per-account extras one venue's mount produced, threaded through [`build_node_inner`]'s
/// thirteen arms: engines to append to `extra`, and their reconcile handles keyed by ROUTE KEY.
///
/// It exists because a venue's SECOND account is not a second arm â it is the same arm mounted
/// again â so the thirteen hand-unrolled calls each hand their surplus to one accumulator instead
/// of each growing a binding nobody named.
#[derive(Default)]
struct AccountExtras {
    engines: Vec<(f64, vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>)>,
    recon: Vec<(String, Box<dyn vike_exec::recon::ReconClient>)>,
}

/// Mount every ACTIVE account of one wired market, hand back the DEFAULT account's engine (the
/// binding each arm has always had) and push the rest into `extras`.
///
/// â  `vike_mount::make_engine_accounts` guarantees the default account is FIRST, so `[0]` here is
/// the engine `make_engine_with_legs` returned before this existed â on a box with one account per
/// venue the vector has length one, `extras` never grows, and the assembly below is byte-identical.
///
/// The extra accounts' reconcile handles are keyed by ROUTE KEY (`venue#LABEL`), which is
/// deliberately NOT a `vike_model::VENUES` id: `vike_core`'s per-venue feed-status health gate looks
/// its key up in `recon_feed_statuses` and reads `Healthy` on a miss, which is the same
/// never-blocked treatment the exec-only venues already get. A second account reconciling on the
/// periodic interval regardless of the first account's feed is the correct behaviour â they are
/// different books.
#[allow(clippy::too_many_arguments)]
fn mount_accounts_of(
    market: (&str, &str),
    cfg: &NodeConfig,
    live_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    extras: &mut AccountExtras,
) -> Result<vike_mount::EngineAndRecon, NodeError> {
    let (venue, symbol) = market;
    // ⚠ ONE row per ACCOUNT, derived from the MOUNTS — the default account on this venue's wired
    // symbol, a labelled account on the symbol of the mount that named it. It is derived from
    // `cfg.core_config` rather than passed in so that it cannot disagree with the `armed_live_venues`
    // probe the composition root locked from: that probe reads the same mounts (`mount_accounts`)
    // and `refuse_unarmed_live_venues` fails the node if the two records differ.
    let account_symbols = account_symbols_for(&mount_accounts(&cfg.core_config), venue, symbol);
    let mut mounted = vike_mount::make_engine_accounts(
        venue,
        &account_symbols,
        &declared_legs_for(&cfg.core_config, venue, symbol),
        &cfg.vars,
        live_events,
        live_venues,
        cfg.recon_enabled,
        recon_trigger,
        cfg.properties_rec.clone(),
        cfg.risk_profile.as_ref(),
        Some(&cfg.policy),
    )
    .map_err(NodeError::RiskBudget)?;
    // The default account is first, and there is always at least one â see that function's order
    // contract. `remove(0)` rather than `into_iter().next()` because the tail is still needed.
    let (_, (engine, recon)) = mounted.remove(0);
    for (label, (extra_engine, extra_recon)) in mounted {
        let route_key = extra_engine.route_key.clone();
        // ⚠ The engine's OWN symbol, not the venue's wired one: a labelled account is mounted on
        // the instrument the mount that named it trades, so logging `symbol` here would name the
        // DEFAULT account's market on a line about a different book.
        let account_symbol = extra_engine.symbol.clone();
        tracing::warn!(
            venue,
            symbol = %account_symbol,
            account = %label,
            route_key = %route_key,
            "mounting a SECOND {venue} account â its fills, positions and reconcile are a separate \
             book from the default account's"
        );
        extras.engines.push((cfg.seed_cash, extra_engine));
        if let Some(rc) = extra_recon {
            extras.recon.push((route_key, rc));
        }
    }
    Ok((engine, recon))
}

/// The assembly itself, unchanged by the preflight wiring â split out only so
/// [`build_node_with_preflight`] and [`build_node`] share ONE body.
fn build_node_inner(cfg: NodeConfig) -> Result<Node, NodeError> {
    // Live-venue events (fills/cancels/rejects) must reach the core ingest, but the core â hence
    // its `EventSender` â doesn't exist until `spawn_core_multi` below. Bridge the gap with a
    // standalone event lane: live clients push here now; a forwarder thread relays into the core
    // ingest right after spawn. In pure PAPER mode nothing is built with `live_events`, so the
    // lane closes on the `drop` below and the forwarder exits immediately.
    let (live_events, live_rx) = vike_exec::event_channel(4096);
    let mut live_venues: HashSet<String> = HashSet::new();

    // ⚠ BEFORE ANYTHING IS BUILT: a mount naming an account this box will not arm fails the node,
    // by name. See [`refuse_unarmed_mount_accounts`] for why this one refusal does not degrade.
    let mounts = mount_accounts(&cfg.core_config);
    refuse_unarmed_mount_accounts(&cfg, &mounts)?;

    // The PRE-MOUNT armed set â the same [`armed_live_venues`] answer the composition root claimed
    // its `vike_ops::live_lock` sentinels from, recomputed here from the very inputs the arms below
    // are about to read. Held across the whole assembly so the tail can compare the two records; see
    // [`refuse_unarmed_live_venues`] for why the comparison is the lock's only guarantee that the
    // hand-written probe did not miss an arm.
    let armed = armed_live_venues(&cfg.vars, &cfg.policy);

    // The SECOND-AND-LATER accounts every venue arm below may mount. Empty on a box with one
    // account per venue — which is every box with no `[accounts]` table — so the assembly it feeds
    // is byte-identical there.
    let mut account_extras = AccountExtras::default();

    // Reconciliation-activation Task 7: pre-create the shared reconcile-trigger channel BEFORE
    // any venue exec client spawns below. `ReconDriver` (and the `Sender` its own
    // `reconcile_trigger()` hands out) doesn't exist until `spawn_recon` runs â well after
    // `spawn_core_multi`, in the caller's `recon_driver` block â but each live venue's resync
    // supervisor is spawned SYNCHRONOUSLY inside `make_engine`, right here. So the channel is
    // built here instead: the `Sender` half is cloned into every live Binance/Bybit/OKX venue's
    // resync supervisor (threaded through `make_engine`'s `recon_trigger` param); the
    // `Receiver` half (paired with one more `Sender` clone) is RETURNED in `Node.recon_trigger` and
    // handed to `spawn_recon` by the caller, which ADOPTS this exact pair instead of minting its own
    // (see `vike_core::spawn_recon`'s doc) â so a poke from any venue's reconnect reaches the SAME
    // driver thread `driver.reconcile_trigger()` would otherwise hand out clones of. Gated on
    // the SAME `VIKE_RECONCILE=1` real-process-env flag the caller's `recon_driver` mount reads
    // (`cfg.recon_enabled`); unset (default) => `recon_trigger` stays `None` and every venue below
    // gets `None` â byte-identical to pre-Task-7 behavior (no channel ever built, no `Sender` ever
    // cloned).
    //
    // â  The trigger is NOT the global on/off, which is why every `make_engine` call below ALSO
    // passes `cfg.recon_enabled` as its own argument: the four venues above are the only ones whose
    // supervisors take a reconnect poke, so this file hardcodes `None` for deribit/aster/alpaca/
    // ctrader/ig/oanda/ibkr/polymarket EVEN WITH RECONCILIATION FULLY ON (they reconcile on the
    // periodic interval only). A `make_engine` that inferred the global gate from
    // `recon_trigger.is_some()` would therefore silently stop reconciling those venues â see that
    // function's `recon_enabled` doc.
    let recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)> =
        cfg.recon_enabled.then(std::sync::mpsc::channel);
    let recon_trigger_tx = recon_trigger.as_ref().map(|(tx, _)| tx.clone());

    let (primary, primary_recon) = mount_accounts_of(
        BINANCE_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        recon_trigger_tx.clone(),
        &mut account_extras,
    )?;
    let (bybit_engine, bybit_recon) = mount_accounts_of(
        BYBIT_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        recon_trigger_tx.clone(),
        &mut account_extras,
    )?;
    let (okx_engine, okx_recon) = mount_accounts_of(
        OKX_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        recon_trigger_tx.clone(),
        &mut account_extras,
    )?;
    // Mount the deribit live exec engine so options orders (from the chain-click â confirm
    // ticket) route to it. The future-kind fills channel still reconciles option fills via
    // `get_user_trades`; broadening the live fills channel to `any` kind is a documented
    // follow-up. Deribit's `ReconClient` IS wired â but inside the `make_engine` deribit arm (a
    // dedicated authed order-WS), not via `build_recon_client` (see its doc). It is NOT passed the
    // reconnect `recon_trigger` (that last arg is `None` below), so it reconciles on the periodic
    // interval only, not on a reconnect poke.
    let (deribit_engine, deribit_recon) = mount_accounts_of(
        DERIBIT_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // Hyperliquid: LIVE (testnet by default; HYPERLIQUID_MAINNET=1 for mainnet) when the key is
    // in .env, else paper. Perp "BTC" mounted; spot orders route here too. `hl_recon` is `Some`
    // on the live path â `hyperliquid_live_client` builds `HyperliquidReconClient` via its
    // bespoke signer/transport (not the standard-creds `build_recon_client`) and returns it here,
    // so HL joins `recon_clients` below and reconciles on the periodic timer. It ALSO takes the
    // `recon_trigger` now: HL's exec pump pokes it on every WS reconnect so a reconcile pass
    // re-syncs state missed while the socket was down (like the binance/bybit/okx venues).
    let (hl_engine, hl_recon) = mount_accounts_of(
        HYPERLIQUID_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        recon_trigger_tx.clone(),
        &mut account_extras,
    )?;
    // Aster USDâ-M perp (`.P` â fapi). LIVE MAINNET exec when `ASTER_LIVE_*` creds are present
    // (real money â see make_engine's aster arm), else TESTNET, else paper. `aster_recon` is the
    // AsterReconClient (built inline in the aster arm from the resolved env), so aster reconciles
    // under `VIKE_RECONCILE=1`. No reconcile TRIGGER is threaded (aster's spawn takes none) â
    // periodic/startup reconcile only, not event-driven.
    let (aster_engine, aster_recon) = mount_accounts_of(
        ASTER_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // Alpaca US-equity broker ("AAPL"). LIVE exec when the `ALPACA_SANDBOX_*` creds are present,
    // else paper (see make_engine's alpaca arm â an EXEC-ONLY mount; no market_feed, so nothing is
    // inserted into `feeds` below). `alpaca_recon` is the inline-built `ReconClient` (a dedicated
    // Bearer `AlpacaRest` on a SECOND OAuth2 lifecycle, not `build_recon_client`), so alpaca
    // reconciles under `VIKE_RECONCILE=1`. No reconcile TRIGGER is threaded (last arg `None`) â
    // periodic/startup reconcile only, like deribit/aster â and alpaca is NOT in
    // `recon_feed_statuses` below, so its health gate reads Healthy (never blocked), like deribit.
    let (alpaca_engine, alpaca_recon) = mount_accounts_of(
        ALPACA_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // cTrader FX/CFD (EUR/USD demo). LIVE exec when the `CTRADER_*` OAuth creds are present, else
    // paper (see make_engine's ctrader arm â an EXEC-ONLY mount; no market_feed, so nothing is
    // inserted into `feeds` below). `ctrader_recon` is the inline-built `ReconClient` (a dedicated
    // authed protobuf socket, not `build_recon_client`), so ctrader reconciles under
    // `VIKE_RECONCILE=1`. No reconcile TRIGGER is threaded (last arg `None`) â periodic/startup
    // reconcile only, like deribit/aster â and ctrader is NOT in `recon_feed_statuses` below, so
    // its health gate reads Healthy (never blocked), exactly like deribit.
    let (ctrader_engine, ctrader_recon) = mount_accounts_of(
        CTRADER_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // IG (IG Group) FX/CFD â the EUR/USD mini epic on the demo account. LIVE exec when the
    // `IG_DEMO_*` config is present, else paper (see make_engine's ig arm â an EXEC-ONLY mount; no
    // market_feed, so nothing is inserted into `feeds` below). `ig_recon` is the inline-built
    // `ReconClient` (a dedicated logged-in `IgSession`, not `build_recon_client`), so IG joins
    // `recon_clients` below and reconciles under `VIKE_RECONCILE=1`. No reconcile TRIGGER is
    // threaded (last arg `None`) â periodic/startup reconcile only, like deribit/aster/alpaca/
    // ctrader â and IG is NOT in `recon_feed_statuses` below, so its health gate reads Healthy
    // (never blocked), like alpaca/ctrader.
    let (ig_engine, ig_recon) = mount_accounts_of(
        IG_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // OANDA v20 FX â EUR/USD on the demo/fxPractice account. LIVE exec when the `OANDA_DEMO_*`
    // config is present, else paper (see make_engine's oanda arm â an EXEC-ONLY mount; no
    // market_feed). `oanda_recon` is the inline-built `ReconClient` (a dedicated Bearer `OandaRest`
    // on its own `/summary`-probed session, not `build_recon_client`), so OANDA joins
    // `recon_clients` below and reconciles under `VIKE_RECONCILE=1`. Interval-only reconcile (last
    // arg `None`), like ig/alpaca/ctrader; NOT in `recon_feed_statuses` â health gate reads Healthy.
    let (oanda_engine, oanda_recon) = mount_accounts_of(
        OANDA_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    // IBKR US-equity broker ("AAPL.SMART.USD" â the canonical `symbol.exchange.currency` form
    // `parse_simplified` maps to a conId), the ONE FEATURE-GATED live venue. This whole call is
    // compiled ONLY under the `ibkr` feature (â `vike-mount/ibkr` â the vendored ibapi
    // tree); with the feature OFF (the default) it does not exist, the ibkr `make_engine` arm
    // falls through to paper, and no ibapi is built â byte-identical to today. LIVE exec when the
    // `IBKR_DEMO_*` config resolves AND a TWS/IB Gateway is running, else paper (an EXEC-ONLY
    // mount â no market_feed, so nothing is inserted into `feeds` below). `ibkr_recon` is the
    // inline-built cpapi `IbkrReconClient` (its OWN dedicated CP-Gateway transport, not
    // `build_recon_client`), so ibkr joins `recon_clients` below and reconciles under
    // `VIKE_RECONCILE=1` as venue #9. No reconcile TRIGGER is threaded (last arg `None`) â
    // periodic/startup reconcile only, like deribit/aster/alpaca â and ibkr is NOT in
    // `recon_feed_statuses` below, so its health gate reads Healthy (never blocked), like
    // deribit/alpaca.
    #[cfg(feature = "ibkr")]
    let (ibkr_engine, ibkr_recon) = mount_accounts_of(
        IBKR_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    let extra = vec![
        (cfg.seed_cash, bybit_engine),
        (cfg.seed_cash, okx_engine),
        (cfg.seed_cash, hl_engine),
        (cfg.seed_cash, aster_engine),
        (cfg.seed_cash, deribit_engine),
        (cfg.seed_cash, alpaca_engine),
        (cfg.seed_cash, ctrader_engine),
        (cfg.seed_cash, ig_engine),
        (cfg.seed_cash, oanda_engine),
    ];
    // Append the feature-gated ibkr engine (rebind-to-mut so the default build's immutable
    // `extra` above stays byte-identical â the two `#[cfg]` lines simply vanish with the feature
    // off; changing mutability means clippy's `redundant_locals` does not fire).
    #[cfg(feature = "ibkr")]
    let mut extra = extra;
    #[cfg(feature = "ibkr")]
    extra.push((cfg.seed_cash, ibkr_engine));
    // Polymarket (this crate's `polymarket` feature â vike-mount's own). The symbol is EMPTY on
    // purpose: this venue's mount is ACCOUNT-WIDE â exec, the user-WS fill pump and reconcile all
    // key off the wallet, not a market â and the arm reads the symbol only to seed its neg-risk
    // lookup, which resolves per token on demand anyway. Mounting one hardcoded 5-minute window
    // would be actively wrong: `btc-updown-5m-*` markets expire every 300s, so any literal here
    // is stale within the minute.
    // â  The feature only makes this COMPILE. What it mounts is decided inside the arm by
    // `POLY_EXEC=1` / `POLY_RECONCILE=1`, both default-off â paper engine, no network call.
    #[cfg(feature = "polymarket")]
    let (poly_engine, poly_recon) = mount_accounts_of(
        POLYMARKET_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    #[cfg(feature = "polymarket")]
    let mut extra = extra;
    #[cfg(feature = "polymarket")]
    extra.push((cfg.seed_cash, poly_engine));
    // FXCM ForexConnect FX (EUR/USD), the FOURTH feature-gated live venue â and the only one whose
    // live gate is a property of the BINARY. `--features fxcm` forwards to `vike-mount/fxcm`, whose
    // `("fxcm", _)` arm goes live only when the `FXCM_DEMO_*` config resolves AND
    // `vike_fxcm::sdk_linked()` is true; a build without the proprietary ForexConnect SDK compiles
    // the crate's `Unavailable` stub, so that arm REFUSES the live mount with an `error!` and lands
    // on paper. Every CI runner takes the refusing branch, because none of them has the SDK.
    //
    // â  Until now this feature linked the SDK and wired NOTHING: `build_node` had no fxcm call and
    // `WIRED_MARKETS` no fxcm row, so an operator installing the `vike-tradehub-fxcm` release asset
    // reasonably believed FXCM was live and it was not, with no error anywhere. This is the row and
    // the call that close that, and they land in the same branch as the sizing conversion
    // (`crates/bridges/fxcm/src/event_mapper.rs`'s `lots_for`) â deliberately, because before it
    // `qty` was read as a LOT COUNT while every caller sends base units, and wiring the venue
    // without it would have made a thousand-fold oversize reachable with real money.
    //
    // EXEC-ONLY, like alpaca/ctrader/ig/oanda/ibkr: this bridge has no market-data seam at all
    // (`LiveDataCaps::NONE`, `NoPump`), so nothing is inserted into `feeds` below and it is NOT in
    // `recon_feed_statuses` â its health gate reads Healthy. `fxcm_recon` is the inline-built
    // `FxcmReconClient` on its OWN dedicated ForexConnect session, so it joins `recon_clients`
    // under `VIKE_RECONCILE=1`; no reconcile TRIGGER is threaded (`None`) â periodic/startup only.
    //
    // â  RECONCILE IS NOT OPTIONAL IN PRACTICE HERE, and that is this venue's own caps row talking:
    // the async fill lane routes on an in-process map built at accept time, so across a RESTART
    // every re-surfaced trade is unroutable and dropped (at `warn!`). `fetch_fill_reports` reading
    // the same Trades table by the same `trade_id` is what recovers them. Mounting fxcm with
    // `VIKE_RECONCILE` off means accepting the loss of every fill that lands across a restart.
    #[cfg(feature = "fxcm")]
    let (fxcm_engine, fxcm_recon) = mount_accounts_of(
        FXCM_MARKET,
        &cfg,
        &live_events,
        &mut live_venues,
        None,
        &mut account_extras,
    )?;
    #[cfg(feature = "fxcm")]
    let mut extra = extra;
    #[cfg(feature = "fxcm")]
    extra.push((cfg.seed_cash, fxcm_engine));
    // …and the SECOND-AND-LATER accounts, appended once every arm above has run. Rebind-to-mut like
    // the three feature blocks above it, so the default build's `extra` stays what it was; the
    // vector is EMPTY on a box with one account per venue, which makes this line a no-op there.
    let mut extra = extra;
    extra.extend(account_extras.engines);
    // Reconcile handles (audit A1 item 4), one per credentialed venue with a wired `ReconClient`.
    // binance/bybit/okx come from `build_recon_client`; hyperliquid/aster/deribit/alpaca/ctrader/
    // ig/oanda build theirs inline in their `make_engine` arms, and â behind the `ibkr`
    // feature â ibkr does too (appended below). `filter_map` drops the `None`s (paper venues, or a
    // venue whose creds were absent). Consumed by the caller's `recon_driver` mount (Task 6, gated on
    // `VIKE_RECONCILE=1`, after `feeds` is assembled â it needs the per-venue feed statuses, which
    // don't exist until then).
    // ⚠ The keys here are VENUE ids, and they stay venue ids: every entry below is that venue's
    // DEFAULT account, whose route key IS its venue id. The extra accounts are appended at the end
    // under their own route keys (`venue#LABEL`) — see `mount_accounts_of` for why a key outside
    // `vike_model::VENUES` is correct there rather than a bug.
    let recon_clients: Vec<(String, Box<dyn vike_exec::recon::ReconClient>)> = [
        ("binance", primary_recon),
        ("bybit", bybit_recon),
        ("okx", okx_recon),
        ("hyperliquid", hl_recon),
        ("aster", aster_recon),
        ("deribit", deribit_recon),
        ("alpaca", alpaca_recon),
        ("ctrader", ctrader_recon),
        ("ig", ig_recon),
        ("oanda", oanda_recon),
    ]
    .into_iter()
    .filter_map(|(venue, recon)| recon.map(|r| (venue.to_string(), r)))
    .collect();
    // Append the feature-gated ibkr reconcile handle (venue #9), dropping `None` exactly like the
    // `filter_map` above (paper / connect-failed / socket-backend-without-CP-Gateway). Rebind-to-mut
    // so the default build's `recon_clients` above is byte-identical â the `#[cfg]` lines vanish
    // with the feature off.
    #[cfg(feature = "ibkr")]
    let mut recon_clients = recon_clients;
    #[cfg(feature = "ibkr")]
    if let Some(r) = ibkr_recon {
        recon_clients.push(("ibkr".to_string(), r));
    }
    // Same rebind-to-mut shape for the feature-gated polymarket reconcile handle. `None` when
    // `POLY_RECONCILE` is unset or creds are absent, dropped exactly like the `filter_map` above.
    // Not in `recon_feed_statuses` either, so its health gate reads Healthy â interval-only
    // reconcile, like deribit/alpaca/ibkr.
    // â  Run this under `VIKE_RECONCILE_POLICY=quarantine` UNLESS `POLY_EXEC=1` is also on: with
    // exec on paper, `hybrid` auto-applies `PositionDrift` and folds the LIVE account's position
    // into the PAPER engine's books at the venue's avg price. (This comment previously blamed
    // `OrphanLocalOrder` "auto-cancelling every pass" â false: that kind resolves to zero events
    // under every policy. See `crates/vike-exec/tests/recon/recon_policy_pin.rs`.) See CLAUDE.md's
    // polymarket paragraph.
    #[cfg(feature = "polymarket")]
    let mut recon_clients = recon_clients;
    #[cfg(feature = "polymarket")]
    if let Some(r) = poly_recon {
        recon_clients.push(("polymarket".to_string(), r));
    }
    // â¦and the fxcm handle, same rebind-to-mut shape. `None` when the venue landed on paper (no
    // creds, or credentials with no SDK linked â the refusal), or when the second ForexConnect
    // login failed. Not in `recon_feed_statuses` â health gate reads Healthy; interval-only.
    #[cfg(feature = "fxcm")]
    let mut recon_clients = recon_clients;
    #[cfg(feature = "fxcm")]
    if let Some(r) = fxcm_recon {
        recon_clients.push(("fxcm".to_string(), r));
    }
    // …and the SECOND-AND-LATER accounts' handles, keyed by ROUTE KEY. Rebind-to-mut like the
    // feature blocks above; EMPTY on a box with one account per venue, so this is a no-op there.
    let mut recon_clients = recon_clients;
    recon_clients.extend(account_extras.recon);
    // Drop our own handle to the live lane: only the live clients (if any) still hold senders, so
    // the forwarder's `recv` ends exactly when the last live client is torn down.
    drop(live_events);

    let core = vike_core::spawn_core_multi(primary, extra, cfg.core_config);
    // Forwarder: relay live-venue events from the standalone lane into the core ingest. Exits when
    // the lane closes (immediately in PAPER mode; on live-client teardown otherwise). Detached â
    // it self-terminates; the core is stopped explicitly via `shutdown_and_join` on exit.
    //
    // TEARDOWN SAFETY: `core.shutdown_and_join()` stops draining the core ingest, THEN joins the
    // exec threads â the live user-data pumps (whose fills `blocking_send` into `live_rx`). If the
    // forwarder were still `blocking_send`ing into the now-undrained core ingest, `live_rx` could
    // fill, the pump's `blocking_send` would block, and the pump-join would hang forever (0xCâ¦409
    // on exit). So the caller raises `forwarder_stop` BEFORE shutting the core down (while the core
    // still drains, so the forwarder is not already parked in a full send): the forwarder then just
    // DRAINS-and-drops, keeping `live_rx` empty so every pump's `blocking_send` returns and its
    // join completes. Late fills during exit are dropped â the app is closing.
    let forwarder_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let core_events = core.event_sender();
        let stop = forwarder_stop.clone();
        let mut live_rx = live_rx;
        std::thread::Builder::new()
            .name("live-event-forward".into())
            .spawn(move || {
                while let Some(ing) = live_rx.blocking_recv() {
                    if let vike_exec::lanes::Ingest::Event(e) = ing {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            continue; // teardown: keep draining, stop forwarding (see above)
                        }
                        if core_events.blocking_send(e).is_err() {
                            break; // core gone â stop relaying
                        }
                    }
                }
            })
            .map_err(NodeError::ForwarderSpawn)?;
    }
    if !live_venues.is_empty() {
        tracing::warn!(
            "LIVE execution active (credential-gated demo) for: {live_venues:?} â real orders \
             will be placed on those venues' demo accounts"
        );
    }
    // THE LOCK BACKSTOP â a venue that armed outside the set the root locked from refuses the whole
    // node. Runs on EVERY build, paper included (where both sets are empty and it is a no-op).
    refuse_unarmed_live_venues(&armed, &live_venues)?;

    Ok(Node { handle: core, recon_clients, recon_trigger, live_venues, forwarder_stop })
}

#[cfg(test)]
mod tests {
    use super::WIRED_MARKETS;

    /// **THE LOUD REFUSAL fires, and it fires on the arming table the fan-out selects with.**
    ///
    /// [`super::refuse_unarmed_mount_accounts`] is the one refusal in this crate that does not
    /// degrade, and a mutation replacing its whole body with `Ok(())` — an operator's `account =
    /// "ALT"` accepted, no engine built for it, and `vike_core` then resolving the mount onto the
    /// venue's DEFAULT account — was measured GREEN across `vike-run` and `vike-tradehub`. It was
    /// cited in four doc comments and covered by nothing.
    ///
    /// Three cases, because each is a different way to get the wrong answer:
    ///
    /// * an account NO policy line names is refused, and the message names venue, account, symbol
    ///   and the two lines that would arm it — a refusal an operator cannot act on is a crash;
    /// * an account named but resolving PAPER (no `__ALT` credentials behind the ceiling) is
    ///   refused too. This is the case that matters most: `venue_account_arming` answers with a
    ///   row for it, so a check written against "is there a row" rather than "did it ARM" would
    ///   pass it straight through to the default engine;
    /// * an account-LESS mount is never refused, whatever the box arms. Every mount that has ever
    ///   shipped is that one.
    #[test]
    fn a_mount_naming_an_unarmed_account_is_refused_by_name() {
        let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
        let cfg = |vars: std::collections::HashMap<String, String>, policy: crate::MountPolicy| {
            super::NodeConfig {
                vars,
                properties_rec: None,
                seed_cash: 10_000.0,
                recon_enabled: false,
                core_config: vike_core::CoreConfig::default(),
                risk_profile: None,
                policy,
            }
        };
        let mount = |account: Option<vike_model::account_keys::AccountLabel>| {
            vec![super::MountAccount {
                venue: "hyperliquid".to_string(),
                symbol: "BTC".to_string(),
                account,
            }]
        };

        // (1) an account no policy line names.
        let bare = cfg(std::collections::HashMap::new(), crate::MountPolicy::default());
        let err = super::refuse_unarmed_mount_accounts(&bare, &mount(Some(alt.clone())))
            .expect_err("an unarmed account must REFUSE, never fall through to the default one");
        let said = err.to_string();
        for needle in ["hyperliquid", "ALT", "BTC", "policy.accounts"] {
            assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
        }

        // (2) …and one the policy DOES name, whose credentials are absent so it resolved paper.
        // `venue_account_arming` returns a row for it either way — the check is on what it ARMED.
        let named = cfg(
            std::collections::HashMap::new(),
            crate::MountPolicy {
                venues: vike_mount::VenuePolicy::default()
                    .declare("hyperliquid", vike_mount::VenueMode::Demo)
                    .declare_account("hyperliquid", &alt, vike_mount::VenueMode::Demo),
                ..crate::MountPolicy::default()
            },
        );
        assert!(
            super::refuse_unarmed_mount_accounts(&named, &mount(Some(alt.clone()))).is_err(),
            "a ceiling that names an account arms nothing on its own — with no `__ALT` credentials \
             the account resolves PAPER, has no engine, and the mount must still be refused"
        );

        // (3) an account-LESS mount is never refused — the byte-identical path every shipped mount
        // takes, on a box that arms nothing at all.
        assert!(
            super::refuse_unarmed_mount_accounts(&bare, &mount(None)).is_ok(),
            "a mount naming no account resolves the venue's default engine as it always has"
        );
    }

    /// Table sanity (audit F5): one row per venue â a duplicate venue would make [`WIRED_MARKETS`]
    /// ambiguous as the authority downstream completeness tests key on â and every symbol is
    /// non-empty EXCEPT polymarket's deliberately account-wide empty one.
    #[test]
    fn wired_markets_venues_are_unique_and_symbols_shaped() {
        let mut venues: Vec<&str> = WIRED_MARKETS.iter().map(|(v, _)| *v).collect();
        let n = venues.len();
        venues.sort_unstable();
        venues.dedup();
        assert_eq!(venues.len(), n, "duplicate venue row in WIRED_MARKETS");
        for &(venue, symbol) in WIRED_MARKETS {
            if venue == "polymarket" {
                assert!(symbol.is_empty(), "polymarket's mount is account-wide (empty symbol)");
            } else {
                assert!(!symbol.is_empty(), "wired venue {venue} must name its mounted symbol");
            }
        }
    }

    /// Every wired venue is on the canonical roster (`vike_model::VENUES`) â the same tie-in every
    /// capability table's completeness test uses, so a typo'd venue id here cannot silently mount
    /// nothing.
    #[test]
    fn wired_markets_venues_are_on_the_canonical_roster() {
        for &(venue, _) in WIRED_MARKETS {
            assert!(
                vike_model::VENUES.contains(&venue),
                "WIRED_MARKETS names {venue}, which is not in vike_model::VENUES"
            );
        }
    }

    // ---- the declared-leg derivation feeding `vike_mount::make_engine_with_legs` ----

    use vike_core::MountLeg;

    /// THE BYTE-IDENTICAL PROPERTY at the derivation end: a leg-free mount contributes nothing to
    /// ANY venue's engine, so `make_engine_with_legs` gets an empty list and touches nothing. This
    /// is the state of every mount in the workspace today.
    ///
    /// â  NON-VACUOUS only because of the SECOND mount. With a lone leg-free mount the assertion
    /// holds under any implementation that iterates legs at all â including one that ignores
    /// `venue` and `wired_symbol` entirely â so it would discriminate nothing. Pairing it with a
    /// mount that DOES declare legs is what gives it content: a derivation that leaked another
    /// mount's legs onto a leg-free venue fails the first assertion, and the second pins that the
    /// leak is not merely misfiled.
    #[test]
    fn a_leg_free_mount_gets_nothing_even_beside_a_mount_that_declares_legs() {
        let none: Vec<MountLeg> = Vec::new();
        let some = vec![MountLeg::same_venue("ETHUSDT")];
        let mounts = || [("binance", none.as_slice()), ("bybit", some.as_slice())].into_iter();
        assert!(
            super::legs_for_venue(mounts(), "binance", "BTCUSDT").is_empty(),
            "binance declares no legs, so bybit's must not reach it"
        );
        assert_eq!(
            super::legs_for_venue(mounts(), "bybit", "SOLUSDT"),
            vec!["ETHUSDT".to_string()],
            "...while the mount that DOES declare one still gets it"
        );
    }

    /// A SAME-VENUE leg (`MountLeg::same_venue`, no venue named) belongs to the MOUNT's venue and
    /// nowhere else.
    ///
    /// NON-VACUOUS: it asserts the OTHER venue gets nothing as well, so a derivation that ignored
    /// the venue and handed every leg to every engine â which would silently grid a bybit engine
    /// with a binance symbol's tick â fails on the second assertion rather than passing the first.
    #[test]
    fn a_same_venue_leg_reaches_only_its_own_mounts_venue() {
        let legs = vec![MountLeg::same_venue("ETHUSDT")];
        let mounts = || [("binance", legs.as_slice())].into_iter();
        assert_eq!(
            super::legs_for_venue(mounts(), "binance", "BTCUSDT"),
            vec!["ETHUSDT".to_string()]
        );
        assert!(super::legs_for_venue(mounts(), "bybit", "BTCUSDT").is_empty());
    }

    /// A CROSS-VENUE leg (`MountLeg::at`, the xEMM hedge shape) is gridded on the venue it will
    /// actually be SENT to, not on the mount's own â the same resolution `resolve_intent_venue`
    /// applies when routing that leg's orders.
    ///
    /// NON-VACUOUS: the mount's own venue is asserted EMPTY, so a derivation that used
    /// `mount.venue` unconditionally (the obvious wrong reading) fails â and it would fail in the
    /// dangerous direction, gridding the maker engine for a symbol only the taker engine ever sees.
    #[test]
    fn a_cross_venue_leg_is_gridded_on_the_venue_it_routes_to() {
        let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
        let mounts = || [("hyperliquid", legs.as_slice())].into_iter();
        assert_eq!(
            super::legs_for_venue(mounts(), "okx", "ETH-USDT-SWAP"),
            vec!["BTC-USDT-SWAP".to_string()]
        );
        assert!(super::legs_for_venue(mounts(), "hyperliquid", "BTC").is_empty());
    }

    /// The venue's OWN wired symbol never becomes a leg: the engine's scalars already are that
    /// symbol's grid, and a second row could only drift from them. This is also the live xEMM
    /// mount's shape today â `XemmMountConfig::validate` refuses any hedge symbol the taker engine
    /// is not already wired for, so its one declared leg lands here and is dropped.
    #[test]
    fn the_venues_own_wired_symbol_is_never_a_leg() {
        let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
        let mounts = [("hyperliquid", legs.as_slice())];
        assert!(super::legs_for_venue(mounts.into_iter(), "okx", "BTC-USDT-SWAP").is_empty());
    }

    /// Two mounts declaring the SAME symbol on one venue produce ONE entry, in declaration order,
    /// and a blank symbol produces none. On a venue whose grid lookup is a network call the
    /// duplicate would be a wasted round trip; the ordering matters because `grid_by_symbol` is an
    /// `IndexMap` whose insertion order reaches the serialized limits.
    #[test]
    fn repeats_collapse_and_blanks_are_dropped_in_declaration_order() {
        let a = vec![MountLeg::same_venue("ETHUSDT"), MountLeg::same_venue("   ")];
        let b = vec![MountLeg::same_venue("SOLUSDT"), MountLeg::same_venue("ETHUSDT")];
        let mounts = [("binance", a.as_slice()), ("binance", b.as_slice())];
        assert_eq!(
            super::legs_for_venue(mounts.into_iter(), "binance", "BTCUSDT"),
            vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]
        );
    }
}
