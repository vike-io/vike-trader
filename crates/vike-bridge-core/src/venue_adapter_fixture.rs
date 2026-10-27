//! The test doubles for the venue-adapter contract, in three parts. `test-support`-gated like
//! `scripted`, so no shipped build compiles them.
//!
//! * [`AdapterFixture`] — owned inputs a test borrows `AdapterInputs` / `AdapterRequest` from.
//! * [`found_tier_events`] — the diagnostics that carry `found_tier`, among the lines a start
//!   logged. A bridge's start test reads those lines with `vike_log::capture::captured`, the
//!   workspace's ONE scoped `tracing` capture (its module doc carries the guarantee against a
//!   sibling test running the same code uncaptured). This crate's `test-support` turns it on — the
//!   one reason this crate takes `vike-log`, as an optional dependency only that feature enables.
//! * [`PlantedAdapter`] and [`PLANTED_DECLARATION`] — the ONE planted contract row, and the ONE
//!   baseline declaration every planted venue in the tree starts from. A test that plants a venue
//!   (`vike-tradehub-core`'s contract, node, clock and startup tests; this crate's own object-safety pin)
//!   overrides the single fact it is about — `VenueDeclaration { addresses_accounts: true,
//!   ..PLANTED_DECLARATION }` — so a field added to [`VenueDeclaration`] is given its test default
//!   HERE, once, rather than in a full struct literal per planted venue. A planted venue whose
//!   `resolve`/`start` carry state of their own (counters, a log, a scripted phase) is still its
//!   own type, and still builds its declaration from [`PLANTED_DECLARATION`].

use std::collections::HashMap;
use std::time::Duration;

use vike_exec::EventSender;
use vike_log::capture::CapturedEvent;
use vike_model::AccountTier;
use vike_model::HaltAdmit;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_secrets::venue_setting::VenueSettings;

use crate::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockDecl, CredentialProbe,
    DeclaredGridSource, ProcessExclusive, ProcessFacts, Resolution, VenueAdapter, VenueDeclaration,
};
use vike_secrets::account_directory::AccountDirectory;

/// Owned inputs. Fields are public: a test sets `account` for a labelled account or `accounts` for
/// a read store.
pub struct AdapterFixture {
    /// The deployment's NAMED values (`AdapterInputs::secrets`): folded flags, attribution codes,
    /// tier-less deployment rows.
    pub vars: HashMap<String, String>,
    /// The started account's credentials, by FIELD (`AdapterInputs::credentials`).
    pub fields: CredentialFields,
    /// The venue's application credentials, by FIELD (cTrader's OAuth client).
    pub venue_fields: CredentialFields,
    /// The `account.id` the fields belong to — `Some(1)` unless a test says otherwise.
    pub account_id: Option<i64>,
    pub account: AccountLabel,
    pub settings: VenueSettings,
    pub accounts: AccountDirectory,
    pub process: ProcessFacts,
}

impl AdapterFixture {
    /// A fixture whose started account holds `fields` (`(FIELD, value)` pairs — `("API_KEY",
    /// "k")`), for the default account, with no deployment values, no venue fields and an unread
    /// account table.
    #[must_use]
    pub fn new(fields: &[(&str, &str)]) -> Self {
        AdapterFixture {
            vars: HashMap::new(),
            fields: CredentialFields::from_pairs(fields.iter().copied()),
            venue_fields: CredentialFields::new(),
            account_id: Some(1),
            account: AccountLabel::Default,
            settings: VenueSettings::default(),
            accounts: AccountDirectory::unread(),
            process: ProcessFacts::default(),
        }
    }

    /// The same fixture with the deployment's NAMED values `pairs` (`AdapterInputs::secrets`).
    #[must_use]
    pub fn with_vars(mut self, pairs: &[(&str, &str)]) -> Self {
        self.vars = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
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
            settings: &self.settings,
            tier,
            accounts: &self.accounts,
            process: &self.process,
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
            recon_enabled: false,
            recon_trigger: None,
            properties_rec: None,
            risk_profile: None,
            market_slippage: None,
            halt_admit: HaltAdmit::default(),
        }
    }
}

/// What a planted venue declares unless a test says otherwise: no named accounts, no exclusive
/// resource, no reconnect trigger, no grid to fetch, no book the store could name and no clock to
/// read — nothing a test that is about something else would look at.
///
/// Override the one fact a test is about with struct-update syntax, in a `const`, a `static` or a
/// method body alike: `VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }`. Do
/// NOT respell the other fields: a field added to [`VenueDeclaration`] then costs one edit, here.
pub const PLANTED_DECLARATION: VenueDeclaration = VenueDeclaration {
    addresses_accounts: false,
    process_exclusive: None,
    takes_recon_trigger: false,
    grid_source: DeclaredGridSource::NoGrid,
    book_identity: BookIdentity::Undeterminable { why: "a planted venue names no book" },
    clock: ClockDecl::NotWired { reason: "a planted venue reads no clock", unmeasured_risk: None },
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
    pub declaration: VenueDeclaration,
    pub resolution: Resolution,
    /// The wired clock read, or `None` for the trait's default refusal.
    pub server_time: Option<PlantedClockRead>,
    /// The startup credential probe offered, or `None` for a venue that is not probed.
    pub credential_probe: Option<PlantedProbeOffer>,
}

impl PlantedAdapter {
    /// A row over [`PLANTED_DECLARATION`] with neither optional leg.
    #[must_use]
    pub const fn new(venue: &'static str, resolution: Resolution) -> Self {
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
    fn declaration(&self) -> VenueDeclaration {
        self.declaration
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
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

/// The events among `events` that carry a `found_tier` field — the diagnostic a demo-pinned arm
/// says about a LIVE account it will not trade (`venue_adapter::report_live_tier_not_wired`), as distinct
/// from the start announcement, which carries `tier` and never `found_tier`. `events` is what
/// `vike_log::capture::captured` — the workspace's one scoped `tracing` capture — returned.
#[must_use]
pub fn found_tier_events(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events.iter().filter(|e| e.field("found_tier").is_some()).collect()
}
