//! ibkr's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! socket/cpapi exec client and a dedicated reconcile client on the exec's OWN product
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md, decision 0140).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: its `("ibkr", _)` arm (now
//! [`IbkrVenueAdapter::start`]), its arming-probe row ([`IbkrVenueAdapter::resolve`]) and its clock,
//! book-identity and grid-source rows ([`IbkrVenueAdapter::declaration`]).
//!
//! ⚠ **Compiled only under this crate's `ibkr` umbrella** (`ibkr-socket` + `ibkr-cpapi`): the start
//! reads BOTH backends — the cpapi and socket reconcile clients and the socket grid pre-fetch. A build without
//! the feature has no IBKR start at all, and `vike-tradehub`'s registry says so with a
//! `FeatureAbsent` row: the venue runs paper and the arming screen names the missing feature.
//!
//! IBKR reads the started ACCOUNT's credential FIELDS — `ACCOUNT` (required), `CLIENT_ID`,
//! `DATA_CLIENT_ID`, `RECON_CLIENT_ID`, `MKTDATA_TYPE` (`AdapterInputs::credentials`, decision 0156
//! item 6) — and its tier's `venue.ibkr.<tier>.{host,port,backend,cpapi_url}` rows, NOT the generic
//! `API_KEY`/`API_SECRET` shape. ABSENT `ACCOUNT` ⇒ paper (absent credentials are the live gate).
//! The exec backend (socket by default, or cpapi) is chosen inside
//! [`crate::IbkrExecutionClient::connect`] by `cfg.backend`.
//!
//! # The account's tier picks the arm, and ONE decision (`IbkrVenueAdapter::decide`)
//!
//! An account has ONE tier ([`vike_bridge_core::venue_adapter::AdapterInputs::tier`], its `account`
//! row's) and its fields are that tier's: there is no demo set stored beside a live one, so neither
//! arm reads, falls back to or warns about the other tier's account. `resolve` and `start` are the
//! same function over the same inputs, so the arming screen and the start cannot disagree:
//!
//! 1. **A `live` account arms the LIVE tier** — `Resolution::Armed` at the live tier, bound
//!    `AccountTier::Live`, with a REAL-MONEY warning at start — **only when ALL of these hold**, read
//!    in this order:
//!    * its `ACCOUNT` is stored;
//!    * the settings database's `account` table has an ACTIVE ibkr/live row for this account
//!      (`account.active` is the off switch). A store that cannot say — no table, unreadable, a
//!      process that read none — is NOT "active": real money is never armed on an assumption;
//!    * the LIVE tier's config loads (`venue.ibkr.live.{host,port,backend,cpapi_url}` parse);
//!    * its backend is `socket` or `cpapi`. A `cpapi` LIVE tier arms HELD:
//!      `held_below_live: Some(HeldBelowLive::OrdersHeld)` (below).
//!
//!    A `live` account whose `ACCOUNT` is stored and that one of the other rules refuses stays
//!    PAPER with [`PaperCause::LiveTierNotWired`] (the cause for "present and not used"), and the
//!    start names the rule at `error!`. It never starts the demo tier: its `ACCOUNT` is a live
//!    account.
//! 2. **A `demo` account arms the DEMO tier exactly as it always did** — an `ACCOUNT` whose config
//!    loads on the demo tier's gateway rows answers
//!    `Resolution::Armed { tier: AccountTier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) }`,
//!    byte for byte, whatever the `account` table says of a live row. (`paper` never reaches a
//!    bridge; it reads as `demo` does.)
//! 3. **Otherwise PAPER** with [`PaperCause::NoCredentials`]: no `ACCOUNT`, or a demo config the
//!    loader refuses. No new cause: a shared `PaperCause` is a coordinated change.
//!
//! ⚠ **cpapi LIVE starts with every ORDER HELD** (stage A of
//! `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`). The cpapi backend has NO
//! observed live fill path (`crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`'s
//! `decode_ws_frame` decodes only `sor`; a fill is recovered only by `request_open_orders` on a
//! `StreamResync`), so a real order could be placed and its fill never seen. So a `(cpapi, live)`
//! arm resolves `Armed { tier: AccountTier::Live, held_below_live: Some(HeldBelowLive::OrdersHeld) }`:
//! it connects to the real account, its reconcile client rides the SAME CP Gateway session, and its
//! exec client refuses every order loudly (`orders_held`'s `HeldExec`, a synthesized terminal
//! `OrderRejected` per order), with `vike-tradehub-core` holding it in front as well. The hold is lifted
//! only by the change that adds the cpapi fill decoder from captured live frames (stage B), never
//! by a setting. Former name, kept because decision 0140's Context cites it: the hold replaced
//! `LiveRefusal::CpapiHeld`, which refused a cpapi LIVE tier outright (paper, `LiveTierNotWired`).
//!
//! ⚠ **The reconcile client rides the exec's OWN product, on BOTH tiers** (decision 0140;
//! `start_with` picks it by `cfg.backend`). One IBKR account admits ONE session
//! (`docs/ops/ibkr-socket-gateway.md` §5), so a socket exec gets the socket
//! `crate::recon::socket::IbkrSocketReconClient` — another API client id
//! (the account's `RECON_CLIENT_ID`, default `client_id + 2`) on the same IB Gateway login — and a
//! cpapi exec gets `crate::recon::cpapi::IbkrReconClient` against the same CP Gateway. A cpapi
//! reconcile is never built beside a socket exec: it would evict one session, or read whichever
//! account the CP Gateway holds. Because reconcile now rides the exec's own gateway the LIVE tier
//! builds one too, under the root rollout rule (ibkr stays at `quarantine` until its POSITION
//! report is proven complete on a live account). Like the demo tier there is NO auto-reconnect and
//! NO auto-relogin: a failed connect leaves the account NOT RUNNING with
//! `ArmingBlock::AdapterFailed` (decision 0156, never a paper stand-in); the gatewayhub's
//! supervisor rebuilds a dropped session (`crates/vike-gatewayhub/CLAUDE.md`), and the in-process
//! tradehub needs a restart.
//!
//! ⚠ **A LIVE ACCOUNT THAT DOES NOT ARM IS NAMED, NOT IGNORED.** Whatever the live tier does not
//! do, the log says which and why, in ONE function (`IbkrVenueAdapter::say_what_the_live_tier_is`)
//! that `start` calls for the account it is starting and [`VenueAdapter::report_not_running_account`]
//! calls for a labelled account that is never started. A `live` account written WITHOUT its
//! `ACCOUNT` (a client id, and the account forgotten) is an `error!` naming the field it lacks. The
//! REAL-MONEY announcement of a started live client is a separate, single call
//! (`IbkrVenueAdapter::announce_live_start`).
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT, the same as cTrader's: `connect` is a BLOCKING, FALLIBLE handshake
//! performed synchronously at start (socket: TCP connect + API handshake to a running TWS/Gateway;
//! cpapi: a browser-authenticated Client Portal Gateway). A connect failure leaves the account
//! NOT RUNNING (`AdapterOutcome::paper()` is folded into `ArmingBlock::AdapterFailed` by the
//! host, decision 0156) — there is no in-thread reconnect.

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockDecl, DeclaredGridSource,
    ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter, VenueDeclaration,
    recon_if_enabled,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::SymbolProperties;
use vike_venues::IbkrBackend;

use crate::config::{IbkrConfig, load_ibkr_config_from, required_fields};
use crate::error::IbkrError;
use crate::recon::cpapi::{VENUE, recon_client};

mod orders_held;

/// ibkr's start. `vike_tradehub::registry::REGISTRY` holds `&IbkrVenueAdapter` under that crate's
/// `ibkr` feature.
pub struct IbkrVenueAdapter;

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

/// Why a `live` account's stored `ACCOUNT` does not arm. One variant per rule of the module doc's
/// list after the first, in the order they are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveRefusal {
    Deactivated,
    NoRow,
    Unknowable(&'static str),
    GatewayRefused,
    BackendUnwired,
}

impl LiveRefusal {
    /// The reason, as a clause of the sentences `say_what_the_live_tier_is` speaks. Names settings
    /// and tables, never a credential field's value.
    fn why(self) -> &'static str {
        match self {
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
            LiveRefusal::BackendUnwired => {
                "its backend is `oauth`, which has no backend of its own yet — set \
                 `venue.ibkr.live.backend` to `socket`"
            }
        }
    }
}

/// What a `live` account holds of the LIVE tier and whether it arms.
enum LiveTier {
    /// No `ACCOUNT` stored: the ordinary unconfigured state, silent but for a half-written set.
    Absent,
    /// Stored, and every rule holds. Carries the config the start connects with.
    Arms(IbkrConfig),
    /// Stored, and one rule refuses it.
    Refused(LiveRefusal),
}

/// [`LiveTier`] without the config, for the sentence-speaking half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSummary {
    /// No stored LIVE `ACCOUNT`: a `live` account without one, or an account whose tier is not
    /// `live` (whose `ACCOUNT` is no live account).
    Absent,
    Arms,
    Refused(LiveRefusal),
}

/// What `resolve` and `start` both act on.
enum Plan {
    /// Start the LIVE tier — real funds.
    Live(IbkrConfig),
    /// Start the DEMO (paper-account) tier, as this arm always did.
    Demo(IbkrConfig),
    /// Stay paper, and why.
    Paper(PaperCause),
}

/// A [`Plan`] plus the fact the log needs beside it.
struct Decision {
    plan: Plan,
    live: LiveSummary,
}

impl IbkrVenueAdapter {
    /// This `demo` account's config — `ACCOUNT` present and every optional field parseable — on
    /// the DEMO tier's gateway settings (`AdapterInputs::settings`, decision 0095).
    fn config(inputs: &AdapterInputs<'_>) -> Option<IbkrConfig> {
        load_ibkr_config_from(AccountTier::Demo, &inputs.credentials, inputs.settings)
    }

    /// What the `account` table says about this account's LIVE row — the `active` flag is the
    /// operator's off switch, and a table that cannot be asked is never read as "active".
    fn live_row(inputs: &AdapterInputs<'_>) -> LiveRow {
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
                && r.tier == AccountTier::Live
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

    /// A `live` account's LIVE tier, rule by rule, in the order the module doc lists them.
    fn live_tier(inputs: &AdapterInputs<'_>) -> LiveTier {
        // The required FIELDS alone decide "stored" — `ACCOUNT`, the whole credential (the Gateway
        // holds the login) — never the loader: a live GATEWAY row nobody can parse must not turn a
        // stored live account into "the store holds nothing".
        if !inputs.credentials.missing(&required_fields()).is_empty() {
            return LiveTier::Absent;
        }
        match Self::live_row(inputs) {
            LiveRow::Active => {}
            LiveRow::Deactivated => return LiveTier::Refused(LiveRefusal::Deactivated),
            LiveRow::NoRow => return LiveTier::Refused(LiveRefusal::NoRow),
            LiveRow::Unknowable(why) => return LiveTier::Refused(LiveRefusal::Unknowable(why)),
        }
        let Some(cfg) =
            load_ibkr_config_from(AccountTier::Live, &inputs.credentials, inputs.settings)
        else {
            return LiveTier::Refused(LiveRefusal::GatewayRefused);
        };
        match cfg.backend {
            // `cpapi` arms too, with every order held (`live_orders_held`; decision 0140, stage A).
            IbkrBackend::Socket | IbkrBackend::Cpapi => LiveTier::Arms(cfg),
            IbkrBackend::Oauth => LiveTier::Refused(LiveRefusal::BackendUnwired),
        }
    }

    /// **THE decision** `resolve` and `start` share: the account's tier picks the arm — a `live`
    /// account the LIVE tier when it arms, else paper with the cause named; a `demo` account the
    /// DEMO tier when its config loads, else paper.
    fn decide(inputs: &AdapterInputs<'_>) -> Decision {
        let (plan, live) = match inputs.tier {
            AccountTier::Live => match Self::live_tier(inputs) {
                LiveTier::Arms(cfg) => (Plan::Live(cfg), LiveSummary::Arms),
                // A stored LIVE `ACCOUNT` that a rule refuses: present and unused, never the demo
                // tier — it is a live account.
                LiveTier::Refused(why) => {
                    (Plan::Paper(PaperCause::LiveTierNotWired), LiveSummary::Refused(why))
                }
                LiveTier::Absent => (Plan::Paper(PaperCause::NoCredentials), LiveSummary::Absent),
            },
            // `paper` never reaches a bridge (`vike-tradehub-core` returns first); it reads as `demo` does.
            AccountTier::Demo | AccountTier::Paper => {
                let plan =
                    Self::config(inputs).map_or(Plan::Paper(PaperCause::NoCredentials), Plan::Demo);
                (plan, LiveSummary::Absent)
            }
        };
        Decision { plan, live }
    }

    /// **Everything this arm says about a LIVE account it does not use**, in one place: `start`
    /// calls it for the account it is starting and [`VenueAdapter::report_not_running_account`] for a
    /// labelled account that is never started, so the two cannot word one fact two ways. A
    /// DIAGNOSTIC: it returns nothing and is reached only after the decision, so it cannot move a
    /// venue between paper and live. Every line carries `venue`, `account` and `found_tier` (never
    /// `tier`, which on a start line means the tier that was BOUND —
    /// `crates/vike-ops/tests/venues/live_mount_line_gate.rs`), and no credential value; the only
    /// credential words in it are the FIELD names a half-written set lacks.
    fn say_what_the_live_tier_is(inputs: &AdapterInputs<'_>, decision: &Decision) {
        let account = inputs.account;
        let whose = account
            .text()
            .map_or_else(|| "the default account".to_string(), |l| format!("account {l}"));
        match (&decision.plan, decision.live) {
            // A live account a rule refused: it is why the venue is on paper.
            (Plan::Paper(PaperCause::LiveTierNotWired), LiveSummary::Refused(why)) => {
                tracing::error!(
                    venue = VENUE,
                    account = %account,
                    found_tier = AccountTier::Live.as_str(),
                    "ibkr: a LIVE-tier account is stored for {whose} and it is NOT used: {}. ibkr \
                     stays PAPER: nothing was signed and no order can reach the live account. \
                     `vike-cli secrets list` names what the store holds",
                    why.why()
                );
            }
            // A `live` account with no `ACCOUNT`. If it holds any other field the set is
            // half-written, and the missing field names are the answer (names only, never a value).
            (Plan::Paper(_), LiveSummary::Absent) if inputs.tier == AccountTier::Live => {
                let missing = inputs.credentials.missing(&required_fields());
                if inputs.credentials.holds_any() && !missing.is_empty() {
                    let names =
                        missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
                    tracing::error!(
                        venue = VENUE,
                        account = %account,
                        found_tier = AccountTier::Live.as_str(),
                        missing_keys = %missing.join(", "),
                        "ibkr: a LIVE-tier credential set is stored for {whose} but it is INCOMPLETE \
                         — {names} unset or blank. Completing it does not mount it by itself: the \
                         LIVE tier arms only with an ACTIVE LIVE account row (tier `live`) and \
                         a socket gateway. ibkr stays PAPER and nothing was signed. \
                         To trade the paper account, give it its own account row at tier `demo` \
                         with its paper-account `ACCOUNT`; `vike-cli secrets list` names what the \
                         store holds"
                    );
                }
            }
            _ => {}
        }
    }

    /// **The real-money warning of a started LIVE client** — one call, the only line that announces
    /// the live tier (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` holds the
    /// convention: `venue`, `account`, `tier`, and a message that begins with the prefix an alert
    /// rule matches on). Names the gateway endpoint, which is a setting and not a credential.
    fn announce_live_start(inputs: &AdapterInputs<'_>, cfg: &IbkrConfig) {
        tracing::warn!(
            venue = VENUE,
            account = %inputs.account,
            tier = AccountTier::Live.as_str(),
            backend = ?cfg.backend,
            host = %cfg.host,
            port = cfg.port,
            "⚠ REAL-MONEY: ibkr: LIVE exec client — real orders on a REAL-FUNDS Gateway account \
             (real funds). There is no auto-reconnect and no auto-relogin on this tier: one IBKR \
             account admits one session, and a dead socket latches the venue closed until the host \
             rebuilds it (a restart)"
        );
    }

    /// **The real-money line of a started LIVE client whose orders are HELD** (`(cpapi, live)`,
    /// stage A of decision 0140): the same convention as [`Self::announce_live_start`], because the
    /// session IS the real account (its data and its reconcile are real; only its orders are
    /// refused). Said instead of that line, never beside it.
    fn announce_held_live_start(inputs: &AdapterInputs<'_>, cfg: &IbkrConfig) {
        tracing::warn!(
            venue = VENUE,
            account = %inputs.account,
            tier = AccountTier::Live.as_str(),
            backend = ?cfg.backend,
            cpapi_url = %cfg.cpapi_url,
            "⚠ REAL-MONEY: ibkr: LIVE exec client with every ORDER HELD — connected to a REAL-FUNDS \
             account over the Client Portal Gateway; its data and reconcile run and EVERY order is \
             refused, because cpapi has no observed live fill path (decision 0140, stage A). No \
             setting lifts the hold. There is no auto-reconnect and no auto-relogin on this tier"
        );
    }
}

/// **Whether a LIVE arm on `cfg` holds every order**: stage A of
/// `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`. `cpapi` has no observed
/// live fill path, so its LIVE tier binds, serves data and reconciles, and sends no order
/// (`orders_held`'s `HeldExec`). The one place the rule is written: `resolve` answers
/// `HeldBelowLive::OrdersHeld` off it and `start_with` wraps the client off it, so the two cannot
/// disagree. Only stage B (the fill decoder from captured live frames) changes it.
fn live_orders_held(cfg: &IbkrConfig) -> bool {
    cfg.backend == IbkrBackend::Cpapi
}

impl VenueAdapter for IbkrVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            takes_recon_trigger: false,
            // `crates/bridges/vike-ibkr/src/properties.rs`'s `fetch_ibkr_properties` opens its OWN
            // transient socket connection to TWS/Gateway per call (IBKR publishes no keyless grid
            // endpoint), and is socket-backend-only.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_from`: the `ACCOUNT`
            // field selects the `DU…`/`U…` account every order is placed in.
            book_identity: BookIdentity::Named { name_fields: &["ACCOUNT"], evm_key_fields: &[] },
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

    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match Self::decide(inputs).plan {
            Plan::Live(cfg) => Resolution::Armed {
                tier: AccountTier::Live,
                held_below_live: live_orders_held(&cfg).then_some(HeldBelowLive::OrdersHeld),
            },
            Plan::Demo(_) => Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            Plan::Paper(cause) => Resolution::Paper(cause),
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the connect closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        start_with(
            req,
            crate::recon::socket::socket_recon_client,
            recon_client,
            move |cfg, events| {
                crate::IbkrExecutionClient::connect(cfg, events).map(|client| {
                    Box::new(client.with_halt_path(halt_path)) as Box<dyn ExecutionClient + Send>
                })
            },
            crate::fetch_ibkr_properties,
        )
    }

    /// A labelled account is started only when it armed, so one with no usable account never
    /// reaches `start`; `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, &Self::decide(inputs));
    }
}

/// [`IbkrVenueAdapter::start`]'s body, with its NETWORK steps as parameters — the two reconcile
/// factories (socket and cpapi), the exec connect and the grid pre-fetch — so a test can reach the
/// connected branch, the backend's choice of reconcile client and the connect-failure demotion
/// without a Gateway. `start` passes the real four. The order is the legacy arm's: the reconcile
/// client first, then the connect, then the grid, which only a connected session fetches.
///
/// **The reconcile client is the exec's own product** (decision 0140), on BOTH tiers: `socket`
/// (and `oauth`, which dials the socket today) builds `socket_recon`, `cpapi` builds `cpapi_recon`;
/// the other factory is never called, so a cpapi session is never opened beside a socket exec.
/// A `cpapi` LIVE tier's exec holds every order (module doc).
fn start_with<S, R, C, G>(
    req: AdapterRequest<'_>,
    socket_recon: S,
    cpapi_recon: R,
    connect: C,
    fetch_grid: G,
) -> AdapterOutcome
where
    S: FnOnce(&IbkrConfig, &str) -> Option<Box<dyn ReconClient>>,
    R: FnOnce(&IbkrConfig, &str) -> Option<Box<dyn ReconClient>>,
    C: FnOnce(&IbkrConfig, EventSender) -> Result<Box<dyn ExecutionClient + Send>, IbkrError>,
    G: FnOnce(&IbkrConfig, &str) -> Option<SymbolProperties>,
{
    // A LIVE account that does not arm is the one thing about the store that is said either way:
    // the reason the venue does not run, or the field a half-written live set lacks.
    let decision = IbkrVenueAdapter::decide(&req.inputs);
    IbkrVenueAdapter::say_what_the_live_tier_is(&req.inputs, &decision);
    let (cfg, tier) = match decision.plan {
        Plan::Paper(_) => return AdapterOutcome::paper(),
        Plan::Demo(cfg) => (cfg, AccountTier::Demo),
        Plan::Live(cfg) => (cfg, AccountTier::Live),
    };
    // Reconcile FIRST, on its OWN dedicated connection, so a report fetch never contends the exec
    // transport: on the exec's IB Gateway under `recon_client_id` (socket), or its own HTTP agent
    // against the exec's CP Gateway (cpapi). Either way the SAME session the exec rides, so the
    // LIVE tier gets one too, a `cpapi` LIVE tier whose orders are held included. LAZY: with
    // reconciliation off nothing is dialled. Built BEFORE the connect, so the demotion below drops
    // it. A factory that cannot connect resolves `None` (unwired, one `warn!`) and exec is
    // unaffected. `tier` is the plan's bound tier — `Demo` or `Live`, never `Paper`.
    let held = tier == AccountTier::Live && live_orders_held(&cfg);
    let recon = recon_if_enabled(req.recon_enabled, || match cfg.backend {
        IbkrBackend::Socket | IbkrBackend::Oauth => socket_recon(&cfg, req.symbol),
        IbkrBackend::Cpapi => cpapi_recon(&cfg, req.symbol),
    });
    match connect(&cfg, req.events.clone()) {
        Ok(client) => {
            // The order hold wraps the CONNECTED client, so the session (and the connect failure
            // below) is the same as an unheld start's; only the order doors refuse.
            let client: Box<dyn ExecutionClient + Send> = if held {
                IbkrVenueAdapter::announce_held_live_start(&req.inputs, &cfg);
                Box::new(orders_held::HeldExec {
                    inner: client,
                    events: req.events.clone(),
                    account: req.inputs.account.to_string(),
                })
            } else {
                if tier == AccountTier::Live {
                    IbkrVenueAdapter::announce_live_start(&req.inputs, &cfg);
                } else {
                    tracing::warn!(
                        venue = VENUE,
                        account = %req.inputs.account,
                        tier = AccountTier::Demo.as_str(),
                        backend = ?cfg.backend,
                        "ibkr: config present → LIVE exec client (real orders on the resolved Gateway account)"
                    );
                }
                client
            };
            // Live RiskGate from the venue's REAL grid: one blocking best-effort `contractDetails`
            // pre-fetch over a dedicated throwaway socket connection. On failure — unreachable
            // Gateway, unparseable symbol, empty reply, OR the cpapi backend (the fetch is
            // socket-only) — the permissive default stands.
            let grid = fetch_grid(&cfg, req.symbol);
            AdapterOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client,
                    bound_tier: tier,
                    grid,
                    // The grid's own multiplier (`resolve_multiplier`: the claim, or a unanimous
                    // reply, else `0.0`, which the fold reads as the inert `1.0`) — the deribit
                    // arm's shape, so an E-mini's PnL is booked at 50 per point.
                    contract_size: grid.map(|g| g.contract_size),
                    margin_mode: None,
                    leg_grids: Vec::new(),
                }),
                recon,
                identity: None,
            }
        }
        Err(e) => {
            // The account does not arm: the host folds this outcome into
            // `ArmingBlock::AdapterFailed` (decision 0156). The reconcile client built moments ago
            // would reconcile nothing against LIVE state — drop it before the log line, the order
            // the legacy arm had, like every paper venue.
            drop(recon);
            tracing::warn!(
                error = %e,
                "ibkr: exec connect failed → the account is NOT RUNNING (decision 0156)"
            );
            if tier == AccountTier::Live {
                tracing::error!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    found_tier = AccountTier::Live.as_str(),
                    "ibkr: the LIVE gateway connect failed, so this ibkr account is NOT RUNNING \
                     (decision 0156). There is no retry and no relogin — one IBKR account admits \
                     one session — so fix the gateway and restart the host"
                );
            }
            AdapterOutcome::paper()
        }
    }
}

#[cfg(test)]
mod tests;
