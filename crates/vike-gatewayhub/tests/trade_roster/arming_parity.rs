//! **THE ARMING PARITY GOLDEN (drift register R1, wave 2 W9a): what every roster venue says about
//! every account, at every tier, in every credential state — recorded BEFORE the venue seam, and
//! held byte-identical AFTER it.**
//!
//! The seam moves the venue half of `vike-tradehub-core` behind a `VenueBackend` (the arming table,
//! the preflight reads, the adapter start). Its one promise is that NO arming outcome moves. This
//! binary is that promise made checkable: it renders, as text, the matrix
//!
//! * `[projection]` — roster venue x credential state x account tier: the policy-driven row
//!   `(tier, effective, block)` (what `vike-backend venues` renders and `make_engine_accounts`
//!   selects with) and the policy-less probe `(effective, block)` handed the SAME fields;
//! * `[scenario]` — the account-table shapes a flat matrix cannot reach: no row, an inactive row, a
//!   tier conflict, a labelled account beside the default (dukascopy's one-sidecar override), a
//!   labelled account alone, a venue-scoped conflict, and the startup preflight's stop
//!   (`PreflightFailed` above `SidecarHeldElsewhere`, `PaperTier` untouched);
//! * `[start]` — each of those, driven through the REAL fan-out (`make_engine_accounts`), reduced to
//!   what an offline run can observe (the pre-connect budget refusal names the venue; a stopped
//!   account carries its block), plus EVERY event it logged (the `ARMING:` lines — the
//!   `NotRunning` log lines — at their level, with their structured fields);
//! * `[box]` — the wired-market list, the armed live set the lock claims from, the accounts the
//!   `flags.tradehub_live` switch stops, and the unaddressable-account report.
//!
//! The text is compared with `tests/arming_parity/golden_*.txt`. The FIXTURES and the golden are
//! the contract; the small `Api` block below is the only part that names how the answer is reached,
//! and it is the one block the seam edits (from the registry-taking functions to a `LocalBackend`).
//! ⚠ A mismatch NEVER rewrites the golden: the whole actual text is printed between
//! `ARMING-PARITY-ACTUAL` markers and the test fails. Re-recording is a reviewed decision, and a
//! test that blessed itself could not fail.
//!
//! ⚠ **Default build, and the three optional venues' build, only.** Under a partial feature set
//! (one or two of `ibkr`, `fxcm`, `polymarket`) there is no golden and the binary compiles to
//! nothing, as `mount_roster.rs` does; the default build records the three as `FeatureAbsent`
//! and the build with all three records `OrdersHeld`, `SdkAbsent` and the exec-flag arm.
//!
//! ⚠ The captured events leave out the `vike_bridge_core::halt` target: that resolver is
//! process-global and prints the box's executable directory, which differs per machine. Every
//! other event is kept — level, message and structured fields; the module-path target is not
//! (a moved module is the same line).
#![cfg(any(
    not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")),
    all(feature = "ibkr", feature = "polymarket", feature = "fxcm")
))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;
use vike_tradehub_core::AdapterPolicy;

// `#[path]` because this file is a test-target CRATE ROOT (see `mount_roster.rs`).
#[path = "support/account_book.rs"]
mod account_book;
#[path = "support/arming_fixtures.rs"]
mod arming_fixtures;
#[path = "support/bridge_backend.rs"]
mod bridge_backend;

use account_book::{book, creds, keyed_policy, with_keyed_account, with_keyed_row};
use arming_fixtures::armed_fields;

#[cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]
const GOLDEN: &str = include_str!("arming_parity/golden_default.txt");
#[cfg(all(feature = "ibkr", feature = "polymarket", feature = "fxcm"))]
const GOLDEN: &str = include_str!("arming_parity/golden_all_venues.txt");

/// The `account.id` a policy-less probe's credentials are filed under — the one row
/// [`keyed_policy`] plants.
const ACCOUNT: i64 = 1;

/// The venues whose arm resolves the account's ROW out of the `account` table, so a policy-less
/// probe (which carries none) answers them `AccountNotInStore` whatever it is handed.
const ROW_RESOLVED: &[&str] = &["dukascopy"];

/// The credential states the flat matrix drives: nothing, a HALF-written set (only the first
/// field), the complete set, and the complete set with the folded `POLY_EXEC` flag on.
#[derive(Clone, Copy)]
enum State {
    None,
    Partial,
    Full,
    FullWithExecFlag,
}

impl State {
    const ALL: [State; 4] = [State::None, State::Partial, State::Full, State::FullWithExecFlag];

    fn word(self) -> &'static str {
        match self {
            State::None => "none",
            State::Partial => "partial",
            State::Full => "full",
            State::FullWithExecFlag => "full+poly_exec",
        }
    }

    fn fields(self, venue: &str) -> Vec<(&'static str, &'static str)> {
        let full = armed_fields(venue);
        match self {
            State::None => Vec::new(),
            State::Partial => full.iter().take(1).copied().collect(),
            State::Full | State::FullWithExecFlag => full.to_vec(),
        }
    }

    fn vars(self) -> HashMap<String, String> {
        match self {
            State::FullWithExecFlag => {
                HashMap::from([(vike_config::flags::POLY_EXEC_ENV.to_string(), "1".to_string())])
            }
            _ => HashMap::new(),
        }
    }
}

fn tw(tier: Option<AccountTier>) -> &'static str {
    tier.map_or("-", AccountTier::as_str)
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

// ============================================================================================
// THE ONE BLOCK THE SEAM EDITS: how each answer is reached. Everything below it is fixtures.
// ============================================================================================

mod api {
    use super::bridge_backend::BridgeBackend;
    use super::*;
    use vike_gatewayhub::registry::trade::REGISTRY;
    use vike_tradehub_core::VenueBackend as _;

    /// The in-process backend over the daemon's registry: the real venue half behind the fold's port.
    fn backend(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> BridgeBackend {
        BridgeBackend::new(REGISTRY, vars.clone(), policy)
    }

    /// The arming snapshot of one assembly.
    fn table(
        vars: &HashMap<String, String>,
        policy: &AdapterPolicy,
    ) -> vike_tradehub_core::ArmingTable {
        backend(vars, policy).arming_table().expect("the local backend always answers")
    }

    /// The per-ACCOUNT rows of one venue, with the startup preflight's stop applied — what
    /// `vike-backend venues` renders and `make_engine_accounts` selects with.
    pub fn rows(
        venue: &str,
        vars: &HashMap<String, String>,
        policy: &AdapterPolicy,
    ) -> Vec<vike_config::VenueArming> {
        vike_tradehub_core::venue_account_arming(&table(vars, policy), venue, Some(policy))
    }

    /// The policy-less probe: the DEFAULT account's `(effective, block)` handed `credentials`.
    pub fn probe(
        venue: &str,
        vars: &HashMap<String, String>,
        credentials: AccountCredentials<'_>,
        tier: AccountTier,
    ) -> (Option<AccountTier>, ArmingBlock) {
        vike_gatewayhub::trade::adapters::arming::venue_arming_under(
            vike_gatewayhub::registry::trade::REGISTRY,
            venue,
            vars,
            credentials,
            tier,
        )
    }

    /// The fan-out of one venue through the REAL start.
    pub fn fan_out(
        venue: &str,
        vars: &HashMap<String, String>,
        policy: &AdapterPolicy,
        account_symbols: &[(AccountLabel, String)],
    ) -> Result<vike_tradehub_core::AccountEngines, vike_tradehub_core::EngineError> {
        let (tx, _rx) = vike_exec::event_channel(16);
        let mut live = HashSet::new();
        let backend = backend(vars, policy);
        let table = backend.arming_table().expect("the local backend always answers");
        let mut env = vike_tradehub_core::AdapterEnv::new(&backend, &table, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(policy);
        vike_tradehub_core::make_engine_accounts(&mut env, venue, account_symbols, &[])
    }

    /// The wired markets a build mounts, as `venue/symbol/rank`.
    pub fn wired() -> Vec<vike_tradehub_core::WiredMarket> {
        table(&HashMap::new(), &AdapterPolicy::default()).wired_markets()
    }

    /// The pre-assembly armed set the live-account locks are claimed from.
    pub fn armed_live(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> Vec<String> {
        vike_tradehub_core::armed_live_venues(&table(vars, policy), policy)
    }

    /// Every account row of every roster venue (what `vike-backend venues` prints).
    pub fn all_rows(
        vars: &HashMap<String, String>,
        policy: &AdapterPolicy,
    ) -> Vec<vike_config::VenueArming> {
        vike_tradehub_core::venue_arming(&table(vars, policy), policy)
    }

    /// The unaddressable-account report text for a policy.
    pub fn unaddressable(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> Option<String> {
        vike_tradehub_core::unaddressable_accounts_message(&table(vars, policy), Some(policy))
    }
}

// ============================================================================================
// The captured log: every event the fan-out emitted, level + message + structured fields, read
// through the workspace's one scoped capture (`vike_log::capture::captured`).
// ============================================================================================

/// The events of `captured`, one text line each, the box-specific halt resolver left out.
fn event_lines(events: &[vike_log::capture::CapturedEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.target != "vike_bridge_core::halt")
        .map(|e| {
            let fields: Vec<String> = e.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{} {} {{{}}}", e.level, e.message, fields.join("; "))
        })
        .collect()
}

// ============================================================================================
// The account-table scenarios.
// ============================================================================================

/// One named account-table shape for `venue`, as a policy.
fn scenarios(venue: &str) -> Vec<(&'static str, AdapterPolicy)> {
    let full = armed_fields(venue).to_vec();
    let default = AccountLabel::Default;
    let mut out: Vec<(&'static str, AdapterPolicy)> = Vec::new();

    out.push(("no-rows", AdapterPolicy::default()));

    // An inactive row holding the complete set.
    let inactive = with_keyed_row(
        AdapterPolicy::default(),
        vike_secrets::Account {
            id: 1,
            venue: venue.to_string(),
            tier: AccountTier::Demo,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
        },
        &full,
    );
    out.push(("inactive-row", inactive));

    // Two ACTIVE rows of one account naming different non-paper tiers.
    let conflict = with_keyed_account(
        with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, &full),
        venue,
        &default,
        AccountTier::Live,
        &[],
    );
    out.push(("tier-conflict", conflict));

    // The default account AND a labelled one, both demo, both holding the complete set.
    let both = with_keyed_account(
        with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, &full),
        venue,
        &alt(),
        AccountTier::Demo,
        &full,
    );
    out.push(("default+alt-demo", both.clone()));

    // A labelled account alone.
    out.push((
        "alt-only-demo",
        with_keyed_account(AdapterPolicy::default(), venue, &alt(), AccountTier::Demo, &full),
    ));

    // The default demo beside a labelled LIVE account: a venue-scoped tier (ibkr) conflicts across
    // labels, every other venue runs both.
    out.push((
        "default-demo+alt-live",
        with_keyed_account(
            with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, &full),
            venue,
            &alt(),
            AccountTier::Live,
            &full,
        ),
    ));

    // The startup preflight's stop, applied the way `assemble_runtime_with_preflight` applies it:
    // the venue's credentials withheld, the venue named in `preflight_failed`.
    let stop = |mut policy: AdapterPolicy| {
        let (kept, _withheld) = policy.accounts.without_venue_credentials(venue);
        policy.accounts = kept;
        policy.preflight_failed.insert(venue.to_string());
        policy
    };
    out.push((
        "preflight-failed-demo",
        stop(with_keyed_account(
            AdapterPolicy::default(),
            venue,
            &default,
            AccountTier::Demo,
            &full,
        )),
    ));
    out.push((
        "preflight-failed-paper",
        stop(with_keyed_account(
            AdapterPolicy::default(),
            venue,
            &default,
            AccountTier::Paper,
            &full,
        )),
    ));
    // The three-way R36 case where the precedence is visible: the failed stop sits ABOVE the
    // one-sidecar override (dukascopy), and the labelled account stops too.
    out.push(("preflight-failed-default+alt-demo", stop(both)));

    // ibkr's cpapi LIVE arm: it arms at `live` with every ORDER HELD (decision 0140, stage A), the
    // one roster row whose resolution names `HeldBelowLive::OrdersHeld`. Its backend is a venue
    // SETTING of the live tier, so the policy carries the `venue_setting` row beside the account.
    if venue == "ibkr" {
        let gateway = [vike_secrets::VenueSettingRow {
            venue: "ibkr".to_string(),
            tier: Some(AccountTier::Live),
            field: "BACKEND".to_string(),
            value: "cpapi".to_string(),
        }];
        let mut held =
            with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Live, &full);
        held.venue_settings = BTreeMap::from([(
            "ibkr".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("ibkr", &gateway),
        )]);
        out.push(("cpapi-live-orders-held", held));
    }
    out
}

// ============================================================================================
// The render.
// ============================================================================================

fn render_rows(out: &mut String, head: &str, rows: &[vike_config::VenueArming]) {
    for r in rows {
        let ids: Vec<String> = r.account_ids.iter().map(i64::to_string).collect();
        writeln!(
            out,
            "{head} venue={} account={} tier={} effective={} block={:?} ids=[{}] why={}",
            r.venue,
            r.label,
            tw(r.tier),
            tw(r.effective),
            r.block,
            ids.join(","),
            r.why()
        )
        .unwrap();
    }
}

fn render_start(
    out: &mut String,
    head: &str,
    venue: &str,
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) {
    let mut symbols = vec![(AccountLabel::Default, "BTCUSDT".to_string())];
    // A labelled row names its own symbol, as the node's `account_symbols_for` hands it.
    if vike_config::account_arming::known_accounts(venue, Some(&policy.accounts)).contains(&alt()) {
        symbols.push((alt(), "BTCUSDT".to_string()));
    }
    let (result, events) =
        vike_log::capture::captured(|| api::fan_out(venue, vars, policy, &symbols));
    let outcome = match result {
        Ok(engines) => {
            let running: Vec<String> = engines
                .running
                .iter()
                .map(|(label, (engine, _recon))| {
                    format!("{label}:{}@{}", engine.route_key, engine.tier.as_str())
                })
                .collect();
            let stopped: Vec<String> = engines
                .not_running
                .iter()
                .map(|n| format!("{}:tier={}:{:?}", n.route_key(), tw(n.tier), n.block))
                .collect();
            format!("ok running=[{}] not_running=[{}]", running.join(","), stopped.join(","))
        }
        Err(vike_tradehub_core::EngineError::MissingRiskBudget { venue: named, .. }) => {
            format!("refused MissingRiskBudget venue={named}")
        }
        Err(vike_tradehub_core::EngineError::NotRunning(n)) => {
            format!("err NotRunning {}:tier={}:{:?}", n.route_key(), tw(n.tier), n.block)
        }
        Err(other) => format!("err {other}"),
    };
    writeln!(out, "{head} {outcome}").unwrap();
    for line in event_lines(&events) {
        writeln!(out, "{head}   log {line}").unwrap();
    }
}

fn render_everything() -> String {
    let mut out = String::new();

    // ---- [projection] ----
    for venue in vike_model::VENUES {
        for state in State::ALL {
            let vars = state.vars();
            let fields = state.fields(venue);
            for tier in AccountTier::ALL {
                let policy = keyed_policy(venue, tier, &fields);
                let rows = api::rows(venue, &vars, &policy);
                let row = rows.iter().find(|r| r.is_default_account()).expect("the default row");
                let probe = if ROW_RESOLVED.contains(venue) {
                    "probe skipped (row-resolved)".to_string()
                } else {
                    let held = book(venue, ACCOUNT, &fields);
                    let (effective, block) =
                        api::probe(venue, &vars, creds(&held, venue, ACCOUNT), tier);
                    format!("probe effective={} block={block:?}", tw(effective))
                };
                writeln!(
                    out,
                    "[projection] {venue} cred={} tier={tier} | row tier={} effective={} \
                     block={:?} | {probe}",
                    state.word(),
                    tw(row.tier),
                    tw(row.effective),
                    row.block,
                )
                .unwrap();
            }
        }
    }

    // ---- [scenario] and [start] ----
    for venue in vike_model::VENUES {
        for (name, policy) in scenarios(venue) {
            render_rows(
                &mut out,
                &format!("[scenario] {venue} {name}"),
                &api::rows(venue, &HashMap::new(), &policy),
            );
            render_start(
                &mut out,
                &format!("[start] {venue} {name}"),
                venue,
                &HashMap::new(),
                &policy,
            );
        }
        // The flat matrix through the real start too: complete set at every tier, and nothing.
        for state in [State::None, State::Full, State::FullWithExecFlag] {
            for tier in AccountTier::ALL {
                render_start(
                    &mut out,
                    &format!("[start] {venue} cred={} tier={tier}", state.word()),
                    venue,
                    &state.vars(),
                    &keyed_policy(venue, tier, &state.fields(venue)),
                );
            }
        }
    }

    // ---- [box] ----
    let wired: Vec<String> = api::wired()
        .iter()
        .map(|m| format!("{}/{}/{}", m.venue, m.symbol, m.engine_rank))
        .collect();
    writeln!(out, "[box] wired_markets {}", wired.join(" ")).unwrap();

    // Every roster venue armed at demo with its complete set, one default account each.
    let everything = vike_model::VENUES.iter().fold(AdapterPolicy::default(), |p, v| {
        with_keyed_account(p, v, &AccountLabel::Default, AccountTier::Demo, armed_fields(v))
    });
    for (name, policy) in [
        ("empty", AdapterPolicy::default()),
        ("everything-demo", everything.clone()),
        (
            "everything-demo+dukascopy-alt",
            with_keyed_account(
                everything,
                "dukascopy",
                &alt(),
                AccountTier::Demo,
                armed_fields("dukascopy"),
            ),
        ),
    ] {
        let vars = HashMap::new();
        writeln!(out, "[box] {name} armed_live {:?}", api::armed_live(&vars, &policy)).unwrap();
        render_rows(&mut out, &format!("[box] {name} venues"), &api::all_rows(&vars, &policy));
        writeln!(out, "[box] {name} unaddressable {:?}", api::unaddressable(&vars, &policy))
            .unwrap();
        let stopped = vike_tradehub_core::stopped_by_the_live_switch(vike_model::VENUES, &policy);
        for n in stopped {
            writeln!(
                out,
                "[box] {name} live-switch-off {}:tier={}:{:?}",
                n.route_key(),
                tw(n.tier),
                n.block
            )
            .unwrap();
        }
    }
    out
}

#[test]
fn every_arming_outcome_is_the_recorded_one() {
    let actual = render_everything();

    // ANTI-VACUITY: the matrix must reach every outcome class it exists to hold, or equality below
    // is between two renders that both say "nothing runs".
    #[allow(unused_mut, clippy::allow_attributes)]
    let mut needles = vec![
        "block=None",
        "block=PaperTier",
        "block=NoCredentials",
        "block=NoAccountRow",
        "block=AccountInactive",
        "block=TierConflict",
        "block=SidecarHeldElsewhere",
        "block=PreflightFailed",
        "block=LiveCredentialsAbsent",
        "refused MissingRiskBudget",
        "ARMING:",
    ];
    // The classes only the three optional venues reach.
    #[cfg(all(feature = "ibkr", feature = "polymarket", feature = "fxcm"))]
    needles.extend(["block=OrdersHeld", "block=SdkAbsent", "block=ExecFlagUnset"]);
    for needle in needles {
        assert!(actual.contains(needle), "the parity matrix never reaches `{needle}`");
    }

    if actual != GOLDEN {
        println!("-----BEGIN ARMING-PARITY-ACTUAL-----");
        print!("{actual}");
        println!("-----END ARMING-PARITY-ACTUAL-----");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush stdout");
        // The lane merges this stream with the build tool's chatter on stderr; let the text drain
        // so a re-recording is not cut mid-line.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let first = actual
            .lines()
            .zip(GOLDEN.lines())
            .position(|(a, g)| a != g)
            .unwrap_or_else(|| actual.lines().count().min(GOLDEN.lines().count()));
        panic!(
            "the arming outcomes differ from the recorded golden, first at line {}:\n  actual:   \
             {:?}\n  recorded: {:?}\n(lines: actual {}, recorded {})",
            first + 1,
            actual.lines().nth(first),
            GOLDEN.lines().nth(first),
            actual.lines().count(),
            GOLDEN.lines().count(),
        );
    }
}
