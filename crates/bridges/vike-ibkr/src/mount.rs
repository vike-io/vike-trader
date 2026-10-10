//! ibkr's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! socket/cpapi exec client and its dedicated cpapi reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: its `("ibkr", _)` arm (now
//! [`IbkrVenueMount::mount`]), its arming-probe row ([`IbkrVenueMount::resolve`]) and its clock,
//! book-identity and grid-source rows ([`IbkrVenueMount::declaration`]).
//!
//! ⚠ **Compiled only under this crate's `ibkr` umbrella** (`ibkr-socket` + `ibkr-cpapi`): the mount
//! reads BOTH backends — the cpapi reconcile client and the socket grid pre-fetch. A build without
//! the feature has no IBKR mount at all, and `vike-tradehub`'s registry says so with a
//! `FeatureAbsent` row: the venue mounts paper and the arming screen names the missing feature.
//!
//! IBKR uses `IBKR_{DEMO|LIVE}_{CLIENT_ID|ACCOUNT|…}` credentials and the tier's
//! `venue.ibkr.<tier>.{host,port,backend,cpapi_url}` rows, NOT the generic `{VENUE}_DEMO_API_KEY`
//! shape. ABSENT ACCOUNT ⇒ paper (absent credentials are the live gate). The exec backend (socket by
//! default, or cpapi) is chosen inside [`crate::IbkrExecutionClient::connect`] by `cfg.backend`.
//!
//! # Two tiers, ONE decision (`IbkrVenueMount::decide`)
//!
//! `resolve` and `mount` are the same function over the same inputs, so the arming screen and the
//! mount cannot disagree:
//!
//! 1. **LIVE arms** — `Resolution::Armed` at the live tier, bound `Tier::Live`, with a REAL-MONEY
//!    warning at mount — **only when ALL of these hold**, read in this order:
//!    * the arming ceiling permits `live` ([`vike_bridge_core::venue_mount::MountInputs::live_permitted`],
//!      read FIRST and only ever a refusal — the pattern binance/bybit/okx/hyperliquid follow; this
//!      arm invents no second gate);
//!    * the LIVE `_ACCOUNT` is stored for this account;
//!    * the settings database's `account` table has an ACTIVE ibkr/live row for this account
//!      (`account.active` is the off switch). A store that cannot say — no table, unreadable, a
//!      process that read none — is NOT "active": real money is never armed on an assumption;
//!    * the LIVE tier's config loads (`venue.ibkr.live.{host,port,backend,cpapi_url}` parse);
//!    * its backend is `socket`.
//! 2. **Otherwise, the DEMO tier arms exactly as it always did** — a demo `_ACCOUNT` whose config
//!    loads answers `Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) }`
//!    under either ceiling, byte for byte. A live account BESIDE it that did not arm is named once
//!    at `warn!` with the reason; a live account that DID arm names the demo account as unused.
//! 3. **Otherwise PAPER**, with a NAMED cause: a LIVE `_ACCOUNT` with no usable demo one answers
//!    [`PaperCause::LiveTierNotWired`] (the cause for "present and not used", `error!` naming the
//!    reason at mount), anything else [`PaperCause::NoCredentials`]. No new cause: a shared
//!    `PaperCause` is a coordinated change.
//!
//! ⚠ **cpapi LIVE is refused, not mounted.** The cpapi backend has NO live fill path
//! (`crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`'s `decode_ws_frame` decodes only
//! `sor`; a fill is recovered only by `request_open_orders` on a `StreamResync`), so a real order
//! could be placed and its fill never seen. The design asks for "connect, serve data, send no real
//! orders"; the shared contract cannot say that — `HeldBelowLive` has only the two DEMO-arm
//! variants and the fold ignores `held_below_live` on a `Tier::Live` arm — so the safe thing this
//! crate can do alone is the stricter one: a cpapi LIVE tier does not arm, the venue stays paper,
//! and the journal says why. The follow-up that would lift it is a coordinated change in
//! `vike-bridge-core`, `vike-mount` and `vike-config` (see `crates/bridges/vike-ibkr/CLAUDE.md`).
//!
//! ⚠ **No reconcile client on the LIVE tier.** The dedicated reconcile client is a cpapi session
//! against `cpapi_url`; one IBKR account admits ONE session (`docs/ops/ibkr-socket-gateway.md` §5),
//! and a CP Gateway that is logged in as another account would answer a live engine with that
//! account's positions. So a LIVE mount builds none (reconcile stays unwired for it, exec
//! unaffected) — and, like the demo tier, there is NO auto-reconnect and NO auto-relogin: a failed
//! connect demotes the venue to PAPER for the session and the repair is a remount.
//!
//! ⚠ **A LIVE-TIER ACCOUNT IS NAMED, NOT IGNORED.** Whatever the live tier does not do, the log says
//! which and why, in ONE function (`IbkrVenueMount::say_what_the_live_tier_is`) that `mount` calls
//! for the account it is mounting and [`VenueMount::report_unmounted_account`] calls for a labelled
//! account that is never mounted. A live tier written WITHOUT its `_ACCOUNT` (a client id, and the
//! account forgotten) is an `error!` naming the key it lacks. The REAL-MONEY announcement of a
//! mounted live client is a separate, single call (`IbkrVenueMount::announce_live_mount`).
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT, the same as cTrader's: `connect` is a BLOCKING, FALLIBLE handshake
//! performed synchronously at mount (socket: TCP connect + API handshake to a running TWS/Gateway;
//! cpapi: a browser-authenticated Client Portal Gateway). A connect failure DEMOTES the venue to
//! PAPER for the whole session — there is no in-thread reconnect.

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, LiveTierSet,
    MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier, VenueDeclaration,
    VenueMount, recon_if_enabled,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;

use crate::config::{
    IbkrBackend, IbkrConfig, live_tier_account_alone, live_tier_account_present,
    load_ibkr_config_for_account, tier_keys,
};
use crate::error::IbkrError;
use crate::recon_client::{VENUE, recon_client};

/// ibkr's mount. `vike_tradehub::registry::REGISTRY` holds `&IbkrVenueMount` under that crate's
/// `ibkr` feature.
pub struct IbkrVenueMount;

/// What the settings database's `account` table says about this account's LIVE row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveRow {
    /// A row for this venue, tier `live` and this label exists and is active.
    Active,
    /// The row exists and the operator switched it off (`account.active`).
    Deactivated,
    /// The table answered and holds no such row.
    NoRow,
    /// The table could not be asked, with the reason (a fixed sentence — never a path or a value).
    Unknowable(&'static str),
}

/// A process that read no settings store (a test, a tool): nothing can say the account is active.
const STORE_UNREAD: &str =
    "this process read no settings store, so nothing says the LIVE account is active";
/// A store with no `account` table (no database, or one older than the table).
const STORE_NO_TABLE: &str = "the settings store carries no `account` table (`vike-cli secrets init` creates it), so nothing says the LIVE account is active";
/// A store that exists and would not answer.
const STORE_UNREADABLE: &str = "the settings store's `account` table would not read, so nothing says the LIVE account is active";

/// Why a stored LIVE account does not arm. One variant per rule of the module doc's list, in the
/// order they are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveRefusal {
    CeilingBelowLive,
    Deactivated,
    NoRow,
    Unknowable(&'static str),
    GatewayRefused,
    CpapiHeld,
    BackendUnwired,
}

impl LiveRefusal {
    /// The reason, as a clause of the sentences `say_what_the_live_tier_is` speaks. Names settings
    /// and tables, never a credential key or a value.
    fn why(self) -> &'static str {
        match self {
            LiveRefusal::CeilingBelowLive => {
                "the arming ceiling `policy.venues.ibkr` is below `live`, and the ceiling only ever \
                 refuses, so nothing below it arms the LIVE tier"
            }
            LiveRefusal::Deactivated => {
                "its `account` row is deactivated (`account.active` is off), which is the off \
                 switch for the LIVE tier"
            }
            LiveRefusal::NoRow => {
                "the settings database has no LIVE ibkr row in its `account` table for it, so \
                 nothing says it is active (`vike-cli secrets accounts` prints the rows)"
            }
            LiveRefusal::Unknowable(why) => why,
            LiveRefusal::GatewayRefused => {
                "its gateway settings (`venue.ibkr.live.*`) do not load: a port that is not a \
                 number, or a backend that is neither `socket` nor `cpapi`"
            }
            LiveRefusal::CpapiHeld => {
                "its backend is `cpapi`, which has NO live fill path (the Client Portal stream \
                 decodes only the `sor` order topic, and a fill is recovered only by an open-orders \
                 request after a stream resync), so a real order could be placed and its fill never \
                 seen: the LIVE tier is HELD BELOW LIVE on `cpapi` — set `venue.ibkr.live.backend` \
                 to `socket`"
            }
            LiveRefusal::BackendUnwired => {
                "its backend is `oauth`, which has no backend of its own yet — set \
                 `venue.ibkr.live.backend` to `socket`"
            }
        }
    }
}

/// What the store holds of this account's LIVE tier and whether it arms.
enum LiveTier {
    /// No LIVE `_ACCOUNT` for this account: the ordinary unconfigured state, and silent.
    Absent,
    /// Stored, and every rule holds. Carries the config the mount connects with.
    Arms(IbkrConfig),
    /// Stored, and one rule refuses it.
    Refused(LiveRefusal),
}

/// [`LiveTier`] without the config, for the sentence-speaking half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSummary {
    Absent,
    Arms,
    Refused(LiveRefusal),
}

/// What `resolve` and `mount` both act on.
enum Plan {
    /// Mount the LIVE tier — real funds.
    Live(IbkrConfig),
    /// Mount the DEMO (paper-account) tier, as this arm always did.
    Demo(IbkrConfig),
    /// Stay paper, and why.
    Paper(PaperCause),
}

/// A [`Plan`] plus the two facts the log needs beside it.
struct Decision {
    plan: Plan,
    live: LiveSummary,
    /// Whether the demo account's config loads (it is mounted, or would be but for the live tier).
    demo_loads: bool,
}

impl IbkrVenueMount {
    /// This account's DEMO config — `_ACCOUNT` present and every optional key parseable — on the
    /// DEMO tier's gateway settings (`MountInputs::settings`, decision 0095).
    fn config(inputs: &MountInputs<'_>) -> Option<IbkrConfig> {
        load_ibkr_config_from_inputs(Environment::Demo, inputs)
    }

    /// What the `account` table says about this account's LIVE row — the `active` flag is the
    /// operator's off switch, and a table that cannot be asked is never read as "active".
    fn live_row(inputs: &MountInputs<'_>) -> LiveRow {
        let Some(rows) = inputs.accounts.rows() else {
            return LiveRow::Unknowable(STORE_UNREAD);
        };
        let Ok(accounts) = rows else {
            return LiveRow::Unknowable(STORE_UNREADABLE);
        };
        let Some(known) = accounts.known() else {
            return LiveRow::Unknowable(STORE_NO_TABLE);
        };
        let mut mine = known.iter().filter(|r| {
            r.venue == VENUE
                && r.tier == Tier::Live.as_str()
                && r.label.as_deref() == inputs.account.text()
        });
        match mine.next() {
            None => LiveRow::NoRow,
            Some(first) => {
                if first.active || mine.any(|r| r.active) {
                    LiveRow::Active
                } else {
                    LiveRow::Deactivated
                }
            }
        }
    }

    /// The LIVE tier, rule by rule, in the order the module doc lists them. The ceiling is read
    /// FIRST and only ever refuses.
    fn live_tier(inputs: &MountInputs<'_>) -> LiveTier {
        if !live_tier_account_present(inputs.account, inputs.secrets) {
            return LiveTier::Absent;
        }
        if !inputs.live_permitted {
            return LiveTier::Refused(LiveRefusal::CeilingBelowLive);
        }
        match Self::live_row(inputs) {
            LiveRow::Active => {}
            LiveRow::Deactivated => return LiveTier::Refused(LiveRefusal::Deactivated),
            LiveRow::NoRow => return LiveTier::Refused(LiveRefusal::NoRow),
            LiveRow::Unknowable(why) => return LiveTier::Refused(LiveRefusal::Unknowable(why)),
        }
        let Some(cfg) = load_ibkr_config_from_inputs(Environment::Live, inputs) else {
            return LiveTier::Refused(LiveRefusal::GatewayRefused);
        };
        match cfg.backend {
            IbkrBackend::Socket => LiveTier::Arms(cfg),
            IbkrBackend::Cpapi => LiveTier::Refused(LiveRefusal::CpapiHeld),
            IbkrBackend::Oauth => LiveTier::Refused(LiveRefusal::BackendUnwired),
        }
    }

    /// **THE decision** `resolve` and `mount` share: LIVE when it arms, else DEMO when its config
    /// loads, else paper with the cause named.
    fn decide(inputs: &MountInputs<'_>) -> Decision {
        let demo = Self::config(inputs);
        let demo_loads = demo.is_some();
        let live = Self::live_tier(inputs);
        let summary = match &live {
            LiveTier::Absent => LiveSummary::Absent,
            LiveTier::Arms(_) => LiveSummary::Arms,
            LiveTier::Refused(why) => LiveSummary::Refused(*why),
        };
        let plan = match (live, demo) {
            (LiveTier::Arms(cfg), _) => Plan::Live(cfg),
            (_, Some(cfg)) => Plan::Demo(cfg),
            // A LIVE `_ACCOUNT` and no demo one: the key alone decides (see
            // `live_tier_account_alone` for why not the loader), and a demo account whose own
            // gateway rows are refused is NOT this cause.
            (_, None) if live_tier_account_alone(inputs.account, inputs.secrets) => {
                Plan::Paper(PaperCause::LiveTierNotWired)
            }
            (_, None) => Plan::Paper(PaperCause::NoCredentials),
        };
        Decision { plan, live: summary, demo_loads }
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `mount` calls it for the
    /// account it is mounting and [`VenueMount::report_unmounted_account`] for a labelled account
    /// that is never mounted, so the two cannot word one fact two ways. A DIAGNOSTIC: it returns
    /// nothing and is reached only after the decision, so it cannot move a venue between paper and
    /// live. Every line carries `venue`, `account` and `found_tier` (never `tier`, which on a mount
    /// line means the tier that was BOUND — `crates/vike-ops/tests/venues/live_mount_line_gate.rs`),
    /// and no credential key or value except the NAMES of keys a half-written set lacks.
    fn say_what_the_live_tier_is(inputs: &MountInputs<'_>, decision: &Decision) {
        let account = inputs.account;
        let whose = account
            .text()
            .map_or_else(|| "the default account".to_string(), |l| format!("account {l}"));
        match (&decision.plan, decision.live) {
            // The live tier armed: the demo account stored beside it is the one left unused.
            (Plan::Live(_), _) => {
                if decision.demo_loads {
                    tracing::warn!(
                        venue = VENUE,
                        account = %account,
                        found_tier = Tier::Demo.as_str(),
                        "ibkr: a DEMO-tier account is ALSO stored for {whose} and it is UNUSED: one \
                         ibkr account mounts ONE tier and the LIVE tier armed, so nothing is placed \
                         on the paper account from this mount"
                    );
                }
            }
            // No usable demo account either: the live account is why the venue is on paper.
            (Plan::Paper(PaperCause::LiveTierNotWired), LiveSummary::Refused(why)) => {
                tracing::error!(
                    venue = VENUE,
                    account = %account,
                    found_tier = Tier::Live.as_str(),
                    "ibkr: a LIVE-tier account is stored for {whose} and it is NOT used: {}. ibkr \
                     stays PAPER: nothing was signed and no order can reach the live account. \
                     `vike-cli secrets list` names what the store holds",
                    why.why()
                );
            }
            // The demo tier mounted (or its own config is the fault) and the live one did not arm.
            (_, LiveSummary::Refused(why)) => {
                tracing::warn!(
                    venue = VENUE,
                    account = %account,
                    found_tier = Tier::Live.as_str(),
                    "ibkr: a LIVE-tier account is ALSO stored for {whose} and it is NOT used: {}. \
                     The paper-account tier is unaffected and nothing was signed with the live set",
                    why.why()
                );
            }
            // No live `_ACCOUNT` at all. If any other live-tier key is stored the set is
            // half-written, and the missing key names are the answer (names only, never a value).
            (Plan::Paper(_), LiveSummary::Absent) if !decision.demo_loads => {
                if let LiveTierSet::Incomplete { missing } =
                    LiveTierSet::read(false, &tier_keys(Environment::Live, account), inputs.secrets)
                {
                    let names =
                        missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
                    tracing::error!(
                        venue = VENUE,
                        account = %account,
                        found_tier = Tier::Live.as_str(),
                        missing_keys = %missing.join(", "),
                        "ibkr: a LIVE-tier credential set is stored for {whose} but it is INCOMPLETE \
                         — {names} unset or blank. Completing it does not mount it by itself: the \
                         LIVE tier arms only with `policy.venues.ibkr` at `live`, an active LIVE \
                         account row and a socket gateway. ibkr stays PAPER and nothing was signed. \
                         To trade the paper account, store its paper-account keys \
                         (`vike-cli secrets set <NAME>`); `vike-cli secrets list` names what the \
                         store holds"
                    );
                }
            }
            _ => {}
        }
    }

    /// **The real-money warning of a mounted LIVE client** — one call, the only line that announces
    /// the live tier (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` holds the
    /// convention: `venue`, `account`, `tier`, and a message that begins with the prefix an alert
    /// rule matches on). Names the gateway endpoint, which is a setting and not a credential.
    fn announce_live_mount(inputs: &MountInputs<'_>, cfg: &IbkrConfig) {
        tracing::warn!(
            venue = VENUE,
            account = %inputs.account,
            tier = Tier::Live.as_str(),
            backend = ?cfg.backend,
            host = %cfg.host,
            port = cfg.port,
            "⚠ REAL-MONEY: ibkr: LIVE exec client — real orders on a REAL-FUNDS Gateway account \
             (real funds). There is no auto-reconnect and no auto-relogin on this tier: one IBKR \
             account admits one session, and a dead socket latches the venue closed until a remount"
        );
    }
}

/// [`load_ibkr_config_for_account`] on this mount's account, secrets and settings, at `env`.
fn load_ibkr_config_from_inputs(env: Environment, inputs: &MountInputs<'_>) -> Option<IbkrConfig> {
    load_ibkr_config_for_account(env, inputs.account, inputs.secrets, inputs.settings)
}

impl VenueMount for IbkrVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no trigger for this venue.
            takes_recon_trigger: false,
            // `crates/bridges/vike-ibkr/src/properties.rs`'s `fetch_ibkr_properties` opens its OWN
            // transient socket connection to TWS/Gateway per call (IBKR publishes no keyless grid
            // endpoint), and is socket-backend-only.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_for_account`: `_ACCOUNT`
            // selects the `DU…`/`U…` account every order is placed in.
            book_identity: BookIdentity::Named {
                prefix: "IBKR",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE"],
                name_suffixes: &["ACCOUNT"],
                evm_key_suffixes: &[],
            },
            // Neither backend offers a public clock: the socket API's time request rides the
            // authenticated TWS socket and the CP Gateway is a LOCAL process, so a "server time"
            // read there would largely be this host comparing itself against itself.
            clock: ClockDecl::NotWired {
                reason: "feature-gated here, and both backends put the clock behind an \
                         authenticated TWS socket / local CP-Gateway session this pre-mount step \
                         does not open",
                unmeasured_risk: None,
            },
        }
    }

    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match Self::decide(inputs).plan {
            Plan::Live(_) => Resolution::Armed { tier: Tier::Live, held_below_live: None },
            Plan::Demo(_) => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            Plan::Paper(cause) => Resolution::Paper(cause),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        // Read BEFORE `req` moves into `mount_with`; the connect closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        mount_with(
            req,
            recon_client,
            move |cfg, events| {
                crate::IbkrExecutionClient::connect(cfg, events).map(|client| {
                    Box::new(client.with_halt_path(halt_path)) as Box<dyn ExecutionClient + Send>
                })
            },
            crate::fetch_ibkr_properties,
        )
    }

    /// A labelled account is mounted only when it armed, so one with no usable account never
    /// reaches `mount`; `vike-mount` asks for what `mount` would have said (see the trait method).
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, &Self::decide(inputs));
    }
}

/// [`IbkrVenueMount::mount`]'s body, with its three NETWORK steps as parameters — the cpapi
/// reconcile factory, the exec connect and the grid pre-fetch — so a test can reach the connected
/// branch and the connect-failure demotion without a Gateway. `mount` passes the real three. The
/// order is the legacy arm's: the reconcile client first, then the connect, then the grid, which
/// only a connected session fetches. The LIVE tier builds no reconcile client (module doc).
fn mount_with<R, C, G>(
    req: MountRequest<'_>,
    build_recon: R,
    connect: C,
    fetch_grid: G,
) -> MountOutcome
where
    R: FnOnce(&IbkrConfig, &str) -> Option<Box<dyn ReconClient>>,
    C: FnOnce(&IbkrConfig, EventSender) -> Result<Box<dyn ExecutionClient + Send>, IbkrError>,
    G: FnOnce(&IbkrConfig, &str) -> Option<SymbolProperties>,
{
    // A LIVE-tier account is the one thing about the store that is said either way: used, unused
    // beside the demo account, or the reason the venue stays paper.
    let decision = IbkrVenueMount::decide(&req.inputs);
    IbkrVenueMount::say_what_the_live_tier_is(&req.inputs, &decision);
    let (cfg, tier) = match decision.plan {
        Plan::Paper(_) => return MountOutcome::paper(),
        Plan::Demo(cfg) => (cfg, Tier::Demo),
        Plan::Live(cfg) => (cfg, Tier::Live),
    };
    // Reconcile FIRST, on its OWN dedicated cpapi `IbkrReconClient`, so a reconcile report fetch
    // never contends the exec transport. It is cpapi-only whatever the exec backend, so with a
    // socket backend and no CP Gateway up it resolves `None` (unwired) and exec is unaffected.
    // LAZY: with reconciliation off the cpapi `tickle`/`secdef_search` handshake is never
    // performed. Built BEFORE the connect, so the demotion below drops it. NEVER on the LIVE tier:
    // a second session on a one-session account, against whichever account the CP Gateway holds.
    let recon = match tier {
        Tier::Live => None,
        Tier::Demo => recon_if_enabled(req.recon_enabled, || build_recon(&cfg, req.symbol)),
    };
    match connect(&cfg, req.events.clone()) {
        Ok(client) => {
            match tier {
                Tier::Live => IbkrVenueMount::announce_live_mount(&req.inputs, &cfg),
                Tier::Demo => tracing::warn!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    tier = Tier::Demo.as_str(),
                    backend = ?cfg.backend,
                    "ibkr: config present → LIVE exec client (real orders on the resolved Gateway account)"
                ),
            }
            // Live RiskGate from the venue's REAL grid: one blocking best-effort `contractDetails`
            // pre-fetch over a dedicated throwaway socket connection. On failure — unreachable
            // Gateway, unparseable symbol, empty reply, OR the cpapi backend (the fetch is
            // socket-only) — the permissive default stands.
            let grid = fetch_grid(&cfg, req.symbol);
            MountOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client,
                    bound_tier: tier,
                    grid,
                    contract_size: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                }),
                recon,
                identity: None,
            }
        }
        Err(e) => {
            // Demote to PAPER for this session. The reconcile client built moments ago would
            // reconcile a PAPER engine against LIVE state — drop it before the log line, the order
            // the legacy arm had, like every paper venue.
            drop(recon);
            tracing::warn!(
                error = %e,
                "ibkr: exec connect failed → falling back to PAPER for this session"
            );
            if tier == Tier::Live {
                tracing::error!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    found_tier = Tier::Live.as_str(),
                    "ibkr: the LIVE gateway connect failed, so ibkr is PAPER for this session. There \
                     is no retry and no relogin — one IBKR account admits one session — so fix the \
                     gateway and remount"
                );
            }
            MountOutcome::paper()
        }
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
