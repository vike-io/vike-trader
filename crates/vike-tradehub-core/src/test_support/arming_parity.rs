//! **The arming parity golden's matrix, fixtures and render** — ONE copy for the two binaries that hold
//! the gatewayhub's golden (`crates/vike-gatewayhub/tests/trade_roster/arming_parity/golden_default.txt`):
//! `crates/vike-gatewayhub/tests/trade_roster/arming_parity.rs` reaches every answer in-process, and
//! `crates/vike-tradehub/tests/hop/venue_backend_parity.rs` reaches it over the wire. Each keeps only its
//! [`ParityApi`] (how an answer is reached, the one block the seam edits); the credential states, the
//! account-table scenarios and the text they render are shared, so the two cannot drift apart.
//!
//! This crate names no venue (decision 0096), so nothing here spells one: each venue's complete credential
//! set, its live-backend settings and the instrument its starts mount come in as the gatewayhub testkit's functions
//! (`vike_gatewayhub::testkit::arming_fixtures::{armed_fields, live_backend_settings, start_symbol}`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use vike_config::ArmingAccount;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use super::account_book::{keyed_policy, with_keyed_account, with_keyed_row};
use crate::{AdapterPolicy, ArmingTable, EngineAccounts, EngineError, RuntimeMarket};

/// The credential states the flat matrix drives: nothing, a HALF-written set (only the first field), the
/// complete set, and the complete set with the `flags.poly_exec` row on.
#[derive(Clone, Copy)]
pub enum State {
    None,
    Partial,
    Full,
    FullWithExecutionFlag,
}

impl State {
    pub const ALL: [State; 4] =
        [State::None, State::Partial, State::Full, State::FullWithExecutionFlag];

    pub fn word(self) -> &'static str {
        match self {
            State::None => "none",
            State::Partial => "partial",
            State::Full => "full",
            State::FullWithExecutionFlag => "full+poly_exec",
        }
    }

    /// The fields this state holds of `full`, the venue's complete set.
    pub fn fields(
        self,
        full: &'static [(&'static str, &'static str)],
    ) -> Vec<(&'static str, &'static str)> {
        match self {
            State::None => Vec::new(),
            State::Partial => full.iter().take(1).copied().collect(),
            State::Full | State::FullWithExecutionFlag => full.to_vec(),
        }
    }

    /// The flags this state runs under: `poly_exec` on for [`State::FullWithExecutionFlag`], every flag off
    /// otherwise.
    pub fn flags(self) -> vike_config::Flags {
        match self {
            State::FullWithExecutionFlag => {
                vike_config::Flags { poly_exec: true, ..vike_config::Flags::default() }
            }
            _ => vike_config::Flags::default(),
        }
    }
}

/// A tier as the golden prints it, `-` for none.
pub fn tw(tier: Option<AccountTier>) -> &'static str {
    tier.map_or("-", AccountTier::as_str)
}

/// The labelled account every scenario beside the default names.
pub fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// One named account-table shape for `venue`, as a policy: `full` is the venue's complete credential set,
/// `live_backend` the settings of a venue whose live arm reads a backend setting (ibkr's cpapi, the one
/// roster row whose resolution names `ArmingHeldBelowLive::OrdersHeld`, decision 0140 stage A).
pub fn scenarios(
    venue: &str,
    full: &[(&str, &str)],
    live_backend: Option<VenueSettings>,
) -> Vec<(&'static str, AdapterPolicy)> {
    let default = AccountLabel::Default;
    let mut out: Vec<(&'static str, AdapterPolicy)> = Vec::new();

    out.push(("no-rows", AdapterPolicy::default()));

    // An inactive row holding the complete set.
    let inactive = with_keyed_row(
        AdapterPolicy::default(),
        vike_secrets::AccountRow {
            id: 1,
            venue: venue.to_string(),
            tier: AccountTier::Demo,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
            symbol: None,
            facts: Default::default(),
        },
        full,
    );
    out.push(("inactive-row", inactive));

    // Two ACTIVE rows of one account naming different non-paper tiers.
    let conflict = with_keyed_account(
        with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, full),
        venue,
        &default,
        AccountTier::Live,
        &[],
    );
    out.push(("tier-conflict", conflict));

    // The default account AND a labelled one, both demo, both holding the complete set.
    let both = with_keyed_account(
        with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, full),
        venue,
        &alt(),
        AccountTier::Demo,
        full,
    );
    out.push(("default+alt-demo", both.clone()));

    // A labelled account alone.
    out.push((
        "alt-only-demo",
        with_keyed_account(AdapterPolicy::default(), venue, &alt(), AccountTier::Demo, full),
    ));

    // The default demo beside a labelled LIVE account: a venue-scoped tier (ibkr) conflicts across
    // labels, every other venue runs both.
    out.push((
        "default-demo+alt-live",
        with_keyed_account(
            with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Demo, full),
            venue,
            &alt(),
            AccountTier::Live,
            full,
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
            full,
        )),
    ));
    out.push((
        "preflight-failed-paper",
        stop(with_keyed_account(
            AdapterPolicy::default(),
            venue,
            &default,
            AccountTier::Paper,
            full,
        )),
    ));
    // The three-way R36 case where the precedence is visible: the failed stop sits ABOVE the
    // one-sidecar override (dukascopy), and the labelled account stops too.
    out.push(("preflight-failed-default+alt-demo", stop(both)));

    // The live arm that holds every ORDER: its backend is a venue SETTING of the live tier, so the
    // policy carries the `venue_setting` row beside the account.
    if let Some(settings) = live_backend {
        let mut held =
            with_keyed_account(AdapterPolicy::default(), venue, &default, AccountTier::Live, full);
        held.venue_settings = BTreeMap::from([(venue.to_string(), settings)]);
        out.push(("cpapi-live-orders-held", held));
    }
    out
}

/// The account rows `rows`, one golden line each under `head`.
pub fn render_rows(out: &mut String, head: &str, rows: &[ArmingAccount]) {
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

/// The accounts a start of `venue` under `policy` is handed: the default on `symbol` (the venue's matrix instrument,
/// by its decision 0290 user name), and the labelled row on its own symbol, as the node's `account_symbols_for` hands it.
pub fn start_symbols(
    venue: &str,
    symbol: &str,
    policy: &AdapterPolicy,
) -> Vec<(AccountLabel, String)> {
    let mut symbols = vec![(AccountLabel::Default, symbol.to_string())];
    if vike_config::account_arming::known_accounts(venue, Some(&policy.accounts)).contains(&alt()) {
        symbols.push((alt(), symbol.to_string()));
    }
    symbols
}

/// The golden's outcome line for one fan-out (`make_engine_accounts`).
pub fn start_outcome(result: Result<EngineAccounts, EngineError>) -> String {
    match result {
        Ok(engines) => {
            let running: Vec<String> = engines
                .running
                .iter()
                .map(|(label, (engine, _reconcile))| {
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
        Err(EngineError::MissingRiskBudget { venue: named, .. }) => {
            format!("refused MissingRiskBudget venue={named}")
        }
        Err(EngineError::NotRunning(n)) => {
            format!("err NotRunning {}:tier={}:{:?}", n.route_key(), tw(n.tier), n.block)
        }
        Err(other) => format!("err {other}"),
    }
}

/// How one parity binary reaches each answer: in-process over the gatewayhub's registry, or over the wire.
/// The table-reading answers are the fold's own functions over [`ParityApi::table`], so a binary spells
/// only how it reaches the table.
pub trait ParityApi {
    /// The arming snapshot of one assembly over these inputs.
    fn table(&self, flags: vike_config::Flags, policy: &AdapterPolicy) -> ArmingTable;

    /// The per-ACCOUNT rows of one venue, with the startup preflight's stop applied.
    fn rows(
        &self,
        venue: &str,
        flags: vike_config::Flags,
        policy: &AdapterPolicy,
    ) -> Vec<ArmingAccount> {
        crate::venue_account_arming(&self.table(flags, policy), venue, Some(policy))
    }

    /// The `[projection]` line's probe column for the default account holding `fields` at `tier`, or
    /// `None` where it does not cross the binary's seam (the column is then left out).
    fn probe(
        &self,
        venue: &str,
        flags: vike_config::Flags,
        fields: &[(&'static str, &'static str)],
        tier: AccountTier,
    ) -> Option<String>;

    /// The fan-out of one venue through the REAL start: its outcome line ([`start_outcome`]) and the log
    /// lines this binary can observe of it (none over the wire).
    fn start(
        &self,
        venue: &str,
        flags: vike_config::Flags,
        policy: &AdapterPolicy,
        account_symbols: &[(AccountLabel, String)],
    ) -> (String, Vec<String>);

    /// The wired markets a build mounts.
    fn wired(&self) -> Vec<RuntimeMarket>;

    /// The pre-assembly armed set the live-account locks are claimed from.
    fn armed_live(&self, flags: vike_config::Flags, policy: &AdapterPolicy) -> Vec<String> {
        crate::armed_live_venues(&self.table(flags, policy), policy)
    }

    /// Every account row of every roster venue (what `vike-backend venues` prints).
    fn all_rows(&self, flags: vike_config::Flags, policy: &AdapterPolicy) -> Vec<ArmingAccount> {
        crate::venue_arming(&self.table(flags, policy), policy)
    }

    /// The unaddressable-account report text for a policy.
    fn unaddressable(&self, flags: vike_config::Flags, policy: &AdapterPolicy) -> Option<String> {
        crate::unaddressable_accounts_message(&self.table(flags, policy), Some(policy))
    }
}

fn render_start(
    api: &impl ParityApi,
    out: &mut String,
    head: &str,
    venue: &str,
    symbol: &str,
    flags: vike_config::Flags,
    policy: &AdapterPolicy,
) {
    let (outcome, log) = api.start(venue, flags, policy, &start_symbols(venue, symbol, policy));
    writeln!(out, "{head} {outcome}").unwrap();
    for line in log {
        writeln!(out, "{head}   log {line}").unwrap();
    }
}

/// The `[projection]`, `[scenario]`, `[start]` and `[box]` text, in the golden's order. `armed_fields` is a
/// venue's complete credential set (empty for an id with no live arm), `live_backend` the settings of a
/// venue whose live arm reads a backend setting, `start_symbol` the instrument a venue's starts mount (its user
/// name), and `sidecar` the venue whose one-sidecar override the last `[box]` policy exercises with a labelled
/// account beside the default.
pub fn render_everything(
    api: &impl ParityApi,
    armed_fields: fn(&str) -> &'static [(&'static str, &'static str)],
    live_backend: fn(&str) -> Option<VenueSettings>,
    start_symbol: fn(&str) -> &'static str,
    sidecar: &str,
) -> String {
    let mut out = String::new();

    // ---- [projection] ----
    for venue in vike_model::VENUES {
        for state in State::ALL {
            let flags = state.flags();
            let fields = state.fields(armed_fields(venue));
            for tier in AccountTier::ALL {
                let policy = keyed_policy(venue, tier, &fields);
                let rows = api.rows(venue, flags, &policy);
                let row = rows.iter().find(|r| r.is_default_account()).expect("the default row");
                write!(
                    out,
                    "[projection] {venue} cred={} tier={tier} | row tier={} effective={} block={:?}",
                    state.word(),
                    tw(row.tier),
                    tw(row.effective),
                    row.block,
                )
                .unwrap();
                if let Some(probe) = api.probe(venue, flags, &fields, tier) {
                    write!(out, " | {probe}").unwrap();
                }
                out.push('\n');
            }
        }
    }

    // ---- [scenario] and [start] ----
    for venue in vike_model::VENUES {
        for (name, policy) in scenarios(venue, armed_fields(venue), live_backend(venue)) {
            render_rows(
                &mut out,
                &format!("[scenario] {venue} {name}"),
                &api.rows(venue, vike_config::Flags::default(), &policy),
            );
            render_start(
                api,
                &mut out,
                &format!("[start] {venue} {name}"),
                venue,
                start_symbol(venue),
                vike_config::Flags::default(),
                &policy,
            );
        }
        // The flat matrix through the real start too: complete set at every tier, and nothing.
        for state in [State::None, State::Full, State::FullWithExecutionFlag] {
            for tier in AccountTier::ALL {
                render_start(
                    api,
                    &mut out,
                    &format!("[start] {venue} cred={} tier={tier}", state.word()),
                    venue,
                    start_symbol(venue),
                    state.flags(),
                    &keyed_policy(venue, tier, &state.fields(armed_fields(venue))),
                );
            }
        }
    }

    // ---- [box] ----
    let wired: Vec<String> =
        api.wired().iter().map(|m| format!("{}/{}", m.venue, m.engine_rank)).collect();
    writeln!(out, "[box] wired_markets {}", wired.join(" ")).unwrap();

    // Every roster venue armed at demo with its complete set, one default account each.
    let everything = vike_model::VENUES.iter().fold(AdapterPolicy::default(), |p, v| {
        with_keyed_account(p, v, &AccountLabel::Default, AccountTier::Demo, armed_fields(v))
    });
    let with_sidecar_alt = with_keyed_account(
        everything.clone(),
        sidecar,
        &alt(),
        AccountTier::Demo,
        armed_fields(sidecar),
    );
    for (name, policy) in [
        ("empty".to_string(), AdapterPolicy::default()),
        ("everything-demo".to_string(), everything),
        (format!("everything-demo+{sidecar}-alt"), with_sidecar_alt),
    ] {
        let flags = vike_config::Flags::default();
        writeln!(out, "[box] {name} armed_live {:?}", api.armed_live(flags, &policy)).unwrap();
        render_rows(&mut out, &format!("[box] {name} venues"), &api.all_rows(flags, &policy));
        writeln!(out, "[box] {name} unaddressable {:?}", api.unaddressable(flags, &policy))
            .unwrap();
        for n in crate::stopped_by_the_live_switch(vike_model::VENUES, &policy) {
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
