//! The test doubles for the venue-adapter contract, in five parts. `test-support`-gated like
//! `scripted`, so no shipped build compiles them.
//!
//! * [`AdapterFixture`] — owned inputs a test borrows `AdapterInputs` / `AdapterRequest` from.
//! * [`found_tier_events`] — the diagnostics that carry `found_tier`, among the lines a start
//!   logged. A bridge's start test reads those lines with `vike_log::capture::captured`, the
//!   workspace's ONE scoped `tracing` capture (its module doc carries the guarantee against a
//!   sibling test running the same code uncaptured). This crate's `test-support` turns vike-log's
//!   own `test-support` (the capture) on.
//! * [`PlantedAdapter`] and [`PLANTED_DECLARATION`] — the ONE planted contract row, and the ONE
//!   baseline declaration every planted venue in the tree starts from. A test that plants a venue
//!   (`vike-tradehub-core`'s contract, node, clock and startup tests; this crate's own object-safety pin)
//!   overrides the single fact it is about — `VenueAdapterDeclaration { addresses_accounts: true,
//!   ..PLANTED_DECLARATION }` — so a field added to [`VenueAdapterDeclaration`] is given its test default
//!   HERE, once, rather than in a full struct literal per planted venue. A planted venue whose
//!   `resolve`/`start` carry state of their own (counters, a log, a scripted phase) is still its
//!   own type, and still builds its declaration from [`PLANTED_DECLARATION`].
//! * [`ExecutionClientInert`] — the client that does nothing, which a bridge's start test hands
//!   its spawn or connect double (eight bridges had each spelled one).
//! * The arming and start ASSERTIONS several bridges had copied word for word
//!   ([`assert_one_key_set_resolves_its_tier`], [`assert_demo_only_resolve`],
//!   [`assert_demo_only_arm_follows_the_tier`], [`assert_never_resolves_live`],
//!   [`assert_quiet_demo_start`]): each bridge keeps its own
//!   `#[test]` and hands in its adapter and its key fixtures, so every venue is still proven.

use std::collections::HashMap;
use std::time::Duration;

use vike_execution::{EventSender, ExecutionClient};
use vike_log::capture::CapturedEvent;
use vike_model::AccountTier;
use vike_model::HaltAdmit;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_secrets::venue_setting::VenueSettings;

use crate::venue_adapter::{
    AdapterFlags, AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly,
    ArmingHeldBelowLive, ArmingNotRunningCause, ArmingResolution, BookIdentityRule,
    ClockDeclaration, CredentialProbe, ProcessExclusive, ProcessPaths, VenueAdapter,
    VenueAdapterDeclaration, VenueAdapterExecutionOutcome,
};
use vike_secrets::account_directory::AccountDirectory;

/// Owned inputs. Fields are public: a test sets `account` for a labelled account or `accounts` for
/// a read store.
pub struct AdapterFixture {
    /// The deployment's NAMED values (`AdapterInputs::secrets`): attribution codes, tier-less deployment
    /// rows.
    pub vars: HashMap<String, String>,
    /// The resolved venue gates (`AdapterInputs::flags`): every one OFF unless a test says otherwise.
    pub flags: AdapterFlags,
    /// The started account's credentials, by FIELD (`AdapterInputs::credentials`).
    pub fields: CredentialFields,
    /// The venue's application credentials, by FIELD (cTrader's OAuth client).
    pub venue_fields: CredentialFields,
    /// The `account.id` the fields belong to — `Some(1)` unless a test says otherwise.
    pub account_id: Option<i64>,
    pub account: AccountLabel,
    pub settings: VenueSettings,
    pub accounts: AccountDirectory,
    pub process: ProcessPaths,
}

impl AdapterFixture {
    /// A fixture whose started account holds `fields` (`(FIELD, value)` pairs — `("API_KEY",
    /// "k")`), for the default account, with no deployment values, no venue fields and an unread
    /// account table.
    #[must_use]
    pub fn new(fields: &[(&str, &str)]) -> Self {
        AdapterFixture {
            vars: HashMap::new(),
            flags: AdapterFlags::default(),
            fields: CredentialFields::from_pairs(fields.iter().copied()),
            venue_fields: CredentialFields::new(),
            account_id: Some(1),
            account: AccountLabel::Default,
            settings: VenueSettings::default(),
            accounts: AccountDirectory::unread(),
            process: ProcessPaths::default(),
        }
    }

    /// The same fixture with the deployment's NAMED values `pairs` (`AdapterInputs::secrets`).
    #[must_use]
    pub fn with_vars(mut self, pairs: &[(&str, &str)]) -> Self {
        self.vars = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        self
    }

    /// The same fixture with the resolved venue gates `flags` (`AdapterInputs::flags`).
    #[must_use]
    pub fn with_flags(mut self, flags: AdapterFlags) -> Self {
        self.flags = flags;
        self
    }

    /// The same fixture with the venue's application fields `fields`.
    #[must_use]
    pub fn with_venue_fields(mut self, fields: &[(&str, &str)]) -> Self {
        self.venue_fields = CredentialFields::from_pairs(fields.iter().copied());
        self
    }

    /// The started account's credentials, as [`Self::inputs`] hands them over.
    #[must_use]
    pub fn credentials(&self) -> AccountCredentials<'_> {
        AccountCredentials::new(self.account_id, Some(&self.fields), Some(&self.venue_fields))
    }

    #[must_use]
    pub fn inputs(&self, tier: AccountTier) -> AdapterInputs<'_> {
        AdapterInputs {
            account: &self.account,
            credentials: self.credentials(),
            secrets: &self.vars,
            flags: self.flags,
            settings: &self.settings,
            tier,
            accounts: &self.accounts,
            process: &self.process,
            fx_clock: None,
        }
    }

    /// A request with reconciliation off, no trigger, no recorder, no profile and the default
    /// halt-admit mode — the offline shape every paper-agreement test uses.
    #[must_use]
    pub fn request<'a>(
        &'a self,
        tier: AccountTier,
        symbol: &'a str,
        events: &'a EventSender,
    ) -> AdapterRequest<'a> {
        AdapterRequest {
            inputs: self.inputs(tier),
            symbol,
            declared_legs: &[],
            events,
            reconcile_enabled: false,
            reconcile_trigger: None,
            properties_rec: None,
            risk_profile: None,
            market_slippage: None,
            halt_admit: HaltAdmit::default(),
        }
    }
}

/// What a planted venue declares unless a test says otherwise: no named accounts, no exclusive
/// resource, no grid to fetch, no book the store could name and no clock to read — nothing a test
/// that is about something else would look at.
///
/// Override the one fact a test is about with struct-update syntax, in a `const`, a `static` or a
/// method body alike: `VenueAdapterDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }`. Do
/// NOT respell the other fields: a field added to [`VenueAdapterDeclaration`] then costs one edit, here.
pub const PLANTED_DECLARATION: VenueAdapterDeclaration = VenueAdapterDeclaration {
    addresses_accounts: false,
    process_exclusive: None,
    grid_source: vike_model::GridSource::NoGrid,
    book_identity: BookIdentityRule::Undeterminable { why: "a planted venue names no book" },
    clock: ClockDeclaration::NotWired {
        reason: "a planted venue reads no clock",
        unmeasured_risk: None,
    },
};

/// A claim on `resource` whose refusals are plain text, for a planted row that only needs to BE
/// exclusive. A planted row that needs to OBSERVE its refusal (a log of every `held_by_another`
/// rendered, say) builds its own `ProcessExclusive`; the function pointers cannot capture.
#[must_use]
pub fn planted_exclusive(resource: &'static str) -> ProcessExclusive {
    ProcessExclusive {
        resource,
        held_by_another: |label, holder| format!("{label} yields to {holder}"),
        already_claimed: |label| format!("{label}: already claimed"),
    }
}

/// A planted row's wired clock read: [`VenueAdapter::server_time_ms`] without `&self` and the timeout.
pub type PlantedClockRead = fn(&AdapterInputs<'_>) -> Result<i64, String>;

/// A planted row's startup probe: [`VenueAdapter::credential_probe`] without `&self`.
pub type PlantedProbeOffer = fn(&AdapterInputs<'_>) -> Option<CredentialProbe>;

/// **The planted contract row**: names `venue`, declares `declaration`, resolves to `resolution`
/// whatever its inputs say, and starts PAPER. The two optional legs default to the trait's own
/// answers — no clock leg (`Err("no clock leg")`) and no startup probe (`None`) — and a test that
/// plants a venue with one of them sets `server_time` / `credential_probe` to a function pointer
/// (a non-capturing closure coerces; a `static` row that needs a counter names its own static from
/// inside the closure).
///
/// Every field is public and `new` is `const`, so a row is `const`-constructible and overridden by
/// struct update — `PlantedAdapter { declaration: …, ..PlantedAdapter::new("planted", resolution) }`
/// — which is what lets it sit in a `static` registry (`&'static dyn VenueAdapter`).
pub struct PlantedAdapter {
    pub venue: &'static str,
    pub declaration: VenueAdapterDeclaration,
    pub resolution: ArmingResolution,
    /// The wired clock read, or `None` for the trait's default refusal.
    pub server_time: Option<PlantedClockRead>,
    /// The startup credential probe offered, or `None` for a venue that is not probed.
    pub credential_probe: Option<PlantedProbeOffer>,
}

impl PlantedAdapter {
    /// A row over [`PLANTED_DECLARATION`] with neither optional leg.
    #[must_use]
    pub const fn new(venue: &'static str, resolution: ArmingResolution) -> Self {
        PlantedAdapter {
            venue,
            declaration: PLANTED_DECLARATION,
            resolution,
            server_time: None,
            credential_probe: None,
        }
    }
}

impl VenueAdapter for PlantedAdapter {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        self.declaration
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        self.resolution
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn server_time_ms(
        &self,
        inputs: &AdapterInputs<'_>,
        _timeout: Duration,
    ) -> Result<i64, String> {
        match self.server_time {
            Some(read) => read(inputs),
            None => Err("no clock leg".into()),
        }
    }
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        self.credential_probe.and_then(|offer| offer(inputs))
    }
}

/// An `ExecutionClient` whose `submit` and `cancel` do nothing and whose every other method is the
/// trait's default: the live client a start test's spawn double returns, or the client of an engine
/// whose events a test feeds by hand. The test judges the start or the fold, never an order.
pub struct ExecutionClientInert;

impl ExecutionClient for ExecutionClientInert {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _order_client_id: &str) {}
}

/// The events among `events` that carry a `found_tier` field — the diagnostic a demo-pinned arm
/// says about a LIVE account it will not trade (`venue_adapter::report_live_tier_not_wired`), as distinct
/// from the start announcement, which carries `tier` and never `found_tier`. `events` is what
/// `vike_log::capture::captured` — the workspace's one scoped `tracing` capture — returned.
#[must_use]
pub fn found_tier_events(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events.iter().filter(|e| e.field("found_tier").is_some()).collect()
}

/// **A one-key-set venue whose tier is the network, at EVERY tier** (the matrix binance, bybit, okx,
/// aster and hyperliquid hand in): `live` is mainnet and binds `Live`; `demo`, and `paper` (which
/// never reaches a bridge and reads as `demo`), bind `Demo`. Each of `absent` (no set, half a set) is
/// `LiveCredentialsAbsent` at `Live` and `NoCredentials` below it; `paired` arms the bound tier with
/// nothing held below live, and a startup probe that records an identity records it at that same
/// tier. The table is spelled out, not derived, so it pins the mapping instead of restating it.
pub fn assert_one_key_set_resolves_its_tier(
    adapter: &dyn VenueAdapter,
    absent: [AdapterFixture; 2],
    paired: &AdapterFixture,
) {
    let table = [
        (AccountTier::Live, AccountTier::Live, ArmingNotRunningCause::LiveCredentialsAbsent),
        (AccountTier::Demo, AccountTier::Demo, ArmingNotRunningCause::NoCredentials),
        (AccountTier::Paper, AccountTier::Demo, ArmingNotRunningCause::NoCredentials),
    ];
    for (tier, bound, cause) in table {
        for fx in &absent {
            assert_eq!(
                adapter.resolve(&fx.inputs(tier)),
                ArmingResolution::NotRunning(cause),
                "{tier}"
            );
        }
        let armed = ArmingResolution::Armed { tier: bound, held_below_live: None };
        assert_eq!(adapter.resolve(&paired.inputs(tier)), armed, "{tier}");
        if let Some(CredentialProbe::RecordsIdentity { bound_tier, .. }) =
            adapter.credential_probe(&paired.inputs(tier))
        {
            assert_eq!(bound_tier, bound, "the startup probe's tier at {tier}");
        }
    }
}

/// **A demo-only arm's arming probe**, pinned for `adapter` (alpaca's and oanda's matrix): the whole
/// `keys` arm DEMO, held below live as `ArmingDemoOnly`, at `Paper` and `Demo`, and are
/// `LiveTierNotWired` at `Live`; no key at all, and each of `partial` (what the loader calls absent),
/// is `NoCredentials` at every tier.
pub fn assert_demo_only_resolve(
    adapter: &dyn VenueAdapter,
    keys: &[(&str, &str)],
    partial: &[&[(&str, &str)]],
) {
    let armed = ArmingResolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(adapter.resolve(&AdapterFixture::new(keys).inputs(tier)), armed, "{tier}");
    }
    assert_eq!(
        adapter.resolve(&AdapterFixture::new(keys).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired),
        "a live account's whole set is one this arm will not use — and the cause says that, rather \
         than that the account holds nothing"
    );
    for tier in AccountTier::ALL {
        assert_eq!(
            adapter.resolve(&AdapterFixture::new(&[]).inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "nothing stored: the ordinary unconfigured state ({tier})"
        );
        for half in partial {
            assert_eq!(
                adapter.resolve(&AdapterFixture::new(half).inputs(tier)),
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
                "half a key set is no key set ({tier})"
            );
        }
    }
}

/// **A demo-only arm's one decision over an account's credentials**, pinned for `arm` (alpaca's
/// `sandbox_arm`, ig's `demo_arm`, fxcm's two-field login): the complete set `fields` arms DEMO below
/// `live` and is `LiveTierNotWired` at `live`; the set without its last field, and no credentials at
/// all, are `Unconfigured` at every tier.
pub fn assert_demo_only_arm_follows_the_tier<C, const N: usize>(
    fields: [&str; N],
    arm: impl Fn(AccountTier, &AccountCredentials<'_>) -> ArmingDemoOnly<C>,
) {
    let full = CredentialFields::from_pairs(fields.map(|f| (f, "x")));
    let half = CredentialFields::from_pairs(fields[..N - 1].iter().map(|f| (*f, "x")));
    let full = AccountCredentials::new(Some(1), Some(&full), None);
    let half = AccountCredentials::new(Some(1), Some(&half), None);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert!(matches!(arm(tier, &full), ArmingDemoOnly::Demo(_)), "{tier}");
    }
    assert!(matches!(arm(AccountTier::Live, &full), ArmingDemoOnly::LiveTierNotWired));
    for tier in AccountTier::ALL {
        assert!(matches!(arm(tier, &half), ArmingDemoOnly::Unconfigured), "{tier}");
        assert!(
            matches!(arm(tier, &AccountCredentials::none()), ArmingDemoOnly::Unconfigured),
            "{tier}"
        );
    }
}

/// **No tier makes a demo-only arm resolve `Live`**: `adapter` over the whole `keys`, at every tier.
pub fn assert_never_resolves_live(adapter: &dyn VenueAdapter, keys: &[(&str, &str)]) {
    for tier in AccountTier::ALL {
        assert!(
            !matches!(
                adapter.resolve(&AdapterFixture::new(keys).inputs(tier)),
                ArmingResolution::Armed { tier: AccountTier::Live, .. }
            ),
            "{tier}"
        );
    }
}

/// **The ordinary demo start, as a demo-pinned arm's start test reads it**: `out` mounts live, bound
/// at DEMO; no line carries `found_tier`; the start announcement carries `tier=demo`; and no captured
/// line holds `secret`, the fixture's credential VALUE.
pub fn assert_quiet_demo_start(out: AdapterOutcome, events: &[CapturedEvent], secret: &str) {
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("a demo account's trio mounts")
    };
    assert_eq!(exec.bound_tier, AccountTier::Demo);
    assert!(found_tier_events(events).is_empty(), "{events:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(secret)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// `venue`'s `venue.<venue>.<tier>.<field>` rows, the shape the composition root reads out of the
/// settings database (decision 0095): each `(field, value)` at `tier`, the field in the store's spelling.
#[must_use]
pub fn venue_settings(venue: &str, tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: venue.to_string(),
            tier: Some(tier.parse().expect("a tier")),
            field: vike_venues::venue_fields::VenueFieldName::normalised(field).store_spelling(),
            value: (*value).to_string(),
        })
        .collect();
    VenueSettings::from_rows(venue, &rows)
}
